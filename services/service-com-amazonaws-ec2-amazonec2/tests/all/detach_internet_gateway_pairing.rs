// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for which ATTACHMENT a `DetachInternetGateway` takes away, driven over the
//! wire the service speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST carrying
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an attachment IS the (internet gateway, VPC) pair, so a detach removes
//! exactly the one attachment both ids of the request name and leaves every other attachment
//! of the account as its reader reported it; afterwards BOTH sides of that pair are free to
//! pair again, and a request naming a pair that is not attached — either side of it belonging
//! to another pair, or the same pair twice — is refused with `Gateway.NotAttached` and removes
//! nothing, as is one naming an id this account holds no resource for, with that resource's own
//! `Invalid<Kind>.NotFound`.
//!
//! The rows differ along what that rule generalises over: how many attached pairs the account
//! holds, which one of them the request names, how many unattached gateways sit beside them,
//! and which side of a named pair (gateway, VPC or both) is the one that does not hold — so no
//! single recorded request is the specification. `delete_internet_gateway.rs` covers the single
//! pair, the routes that follow an attachment and the `DryRun`; this suite covers the PAIRING
//! across a population.

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, InstanceStateName};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, filters};
use super::wire::{child, child_text, document, param, set_items};

/// The Region every call here is signed for. An internet gateway carries no Region member, so
/// the Region only picks which account's ambient network the store holds.
const REGION: &str = "us-east-1";

/// The code EC2 answers a detach of a pair that is not attached with (EC2 API "Error codes").
const GATEWAY_NOT_ATTACHED: &str = "Gateway.NotAttached";

/// One internet gateway as the report carries it, in the members a detach decides: its
/// identity, whether it carries an attachment set at all, and the attachments in that set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedGateway {
    internet_gateway_id: String,
    /// Whether the report carried an `attachmentSet` element for this gateway at all — a
    /// gateway attached to nothing carries a PRESENT and EMPTY one.
    attachment_set_present: bool,
    /// Each attachment as `(state, vpcId)`, in the order the report listed them.
    attachments: Vec<(String, String)>,
}

/// Every internet gateway the account holds, read back through the unnarrowed report, in the
/// order it reported them.
async fn report(router: &MultiServiceRouter, case: &str) -> Vec<ReportedGateway> {
    let (status, body) =
        super::ec2_query::call(router, REGION, "DescribeInternetGateways", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reporting every internet gateway, got {status} with \
         body {body}"
    );
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "internetGatewaySet")
        .into_iter()
        .map(|item| ReportedGateway {
            internet_gateway_id: child_text(item, "internetGatewayId").unwrap_or_default(),
            attachment_set_present: child(item, "attachmentSet").is_some(),
            attachments: set_items(item, "attachmentSet")
                .into_iter()
                .map(|attachment| {
                    (
                        child_text(attachment, "state").unwrap_or_default(),
                        child_text(attachment, "vpcId").unwrap_or_default(),
                    )
                })
                .collect(),
        })
        .collect()
}

/// A VPC created over the wire, by id.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// An internet gateway created over the wire, by id.
async fn create_gateway(router: &MultiServiceRouter) -> String {
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateInternetGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateInternetGateway: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "internetGatewayId")
        .unwrap_or_else(|| panic!("CreateInternetGateway answered no internetGatewayId: {body}"))
}

/// One `AttachInternetGateway` call for the pair, as its HTTP status and XML body.
async fn attach_call(
    router: &MultiServiceRouter,
    internet_gateway_id: &str,
    vpc_id: &str,
) -> (u16, String) {
    let params = [
        param("InternetGatewayId", internet_gateway_id),
        param("VpcId", vpc_id),
    ];
    super::ec2_query::call(router, REGION, "AttachInternetGateway", &params).await
}

/// Attach `internet_gateway_id` to `vpc_id` over the wire, which must succeed.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    internet_gateway_id: &str,
    vpc_id: &str,
) {
    let (status, body) = attach_call(router, internet_gateway_id, vpc_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 attaching {internet_gateway_id} to {vpc_id}, got \
         {status} with body {body}"
    );
}

/// One `DetachInternetGateway` call for the pair, as its HTTP status and XML body.
async fn detach_call(
    router: &MultiServiceRouter,
    internet_gateway_id: &str,
    vpc_id: &str,
) -> (u16, String) {
    let params = [
        param("InternetGatewayId", internet_gateway_id),
        param("VpcId", vpc_id),
    ];
    super::ec2_query::call(router, REGION, "DetachInternetGateway", &params).await
}

/// Detach `internet_gateway_id` from `vpc_id` over the wire, which must succeed.
async fn detach(
    router: &MultiServiceRouter,
    case: &str,
    internet_gateway_id: &str,
    vpc_id: &str,
) {
    let (status, body) = detach_call(router, internet_gateway_id, vpc_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 detaching {internet_gateway_id} from {vpc_id}, got \
         {status} with body {body}"
    );
}

/// The ids of every internet gateway the report narrowed by `attachment.vpc-id` carries — the
/// gateways the account reports as attaching that VPC
/// (`DescribeInternetGatewaysRequest$Filters`, `attachment.vpc-id`).
async fn gateways_attaching(router: &MultiServiceRouter, case: &str, vpc_id: &str) -> Vec<String> {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeInternetGateways",
        &filters(&[("attachment.vpc-id", &[vpc_id])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reporting the gateways attaching {vpc_id}, got \
         {status} with body {body}"
    );
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "internetGatewaySet")
        .into_iter()
        .filter_map(|item| child_text(item, "internetGatewayId"))
        .collect()
}

/// A population of internet gateways and VPCs: `attached_pairs` gateways each attached to a
/// VPC of its own, plus `spare_gateways` attached to nothing.
#[derive(Debug, Clone)]
struct Population {
    attached_pairs: usize,
    spare_gateways: usize,
}

/// The ids this suite's population holds: the gateways in creation order (the attached ones
/// first) and the VPC each attached gateway was paired with.
struct Built {
    gateways: Vec<String>,
    vpcs: Vec<String>,
}

/// Create `population` over the wire and answer the ids it minted.
async fn build(router: &MultiServiceRouter, case: &str, population: &Population) -> Built {
    let mut vpcs = Vec::new();
    let mut gateways = Vec::new();
    for index in 0..population.attached_pairs {
        // Distinct CIDR blocks, one per VPC: a block is the only thing a VPC create needs and
        // two VPCs of one account must not overlap.
        let vpc_id = create_vpc(router, &format!("10.{}.0.0/16", 20 + index)).await;
        let internet_gateway_id = create_gateway(router).await;
        attach(router, case, &internet_gateway_id, &vpc_id).await;
        vpcs.push(vpc_id);
        gateways.push(internet_gateway_id);
    }
    for _ in 0..population.spare_gateways {
        gateways.push(create_gateway(router).await);
    }
    Built { gateways, vpcs }
}

/// One row of the detach table: a population, and which of its attached pairs the request
/// names.
#[derive(Debug, Clone)]
struct DetachCase {
    case: &'static str,
    population: Population,
    /// Which attached pair the detach names, by position.
    target: usize,
}

/// The detach table: the populations and the named pair this suite's first two tests run over.
fn detach_cases() -> Vec<DetachCase> {
    vec![
        DetachCase {
            case: "the only attached pair the account holds",
            population: Population {
                attached_pairs: 1,
                spare_gateways: 0,
            },
            target: 0,
        },
        DetachCase {
            case: "the only attached pair, beside two gateways attached to nothing",
            population: Population {
                attached_pairs: 1,
                spare_gateways: 2,
            },
            target: 0,
        },
        DetachCase {
            case: "the first of two attached pairs",
            population: Population {
                attached_pairs: 2,
                spare_gateways: 0,
            },
            target: 0,
        },
        DetachCase {
            case: "the last of two attached pairs, beside a gateway attached to nothing",
            population: Population {
                attached_pairs: 2,
                spare_gateways: 1,
            },
            target: 1,
        },
        DetachCase {
            case: "the middle of three attached pairs",
            population: Population {
                attached_pairs: 3,
                spare_gateways: 0,
            },
            target: 1,
        },
        DetachCase {
            case: "the last of three attached pairs",
            population: Population {
                attached_pairs: 3,
                spare_gateways: 2,
            },
            target: 2,
        },
    ]
}

/// A detach removes exactly the attachment both ids of the request name: the named gateway
/// reads back with a present and empty attachment set, the VPC reads back as attached by no
/// gateway, and every OTHER gateway of the account — attached or not — reads back exactly as
/// it did before.
#[tokio::test]
async fn a_detach_removes_only_the_attachment_the_pair_names() {
    for row in detach_cases() {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let built = build(&router, case, &row.population).await;
        let target_gateway = built.gateways[row.target].clone();
        let target_vpc = built.vpcs[row.target].clone();

        let before = report(&router, case).await;
        assert!(
            before.iter().any(|gateway| {
                gateway.internet_gateway_id == target_gateway
                    && gateway
                        .attachments
                        .iter()
                        .any(|(state, vpc_id)| state == "available" && *vpc_id == target_vpc)
            }),
            "case {case:?}: expected {target_gateway} to read back attached to {target_vpc} \
             before the detach, got {before:?}"
        );

        detach(&router, case, &target_gateway, &target_vpc).await;

        let after = report(&router, case).await;
        let expected: Vec<ReportedGateway> = before
            .iter()
            .map(|gateway| {
                if gateway.internet_gateway_id == target_gateway {
                    ReportedGateway {
                        // "For an internet gateway, the state is `available` when attached to a
                        // VPC; otherwise, this value is not returned"
                        // (`InternetGatewayAttachment$State`): what goes is the entry naming
                        // this VPC, leaving the set present and empty.
                        attachment_set_present: true,
                        attachments: Vec::new(),
                        ..gateway.clone()
                    }
                } else {
                    gateway.clone()
                }
            })
            .collect();
        assert_eq!(
            after, expected,
            "case {case:?}: expected only the attachment between {target_gateway} and \
             {target_vpc} to be gone, got {after:?}"
        );

        let attaching = gateways_attaching(&router, case, &target_vpc).await;
        assert!(
            attaching.is_empty(),
            "case {case:?}: expected no gateway to report attaching {target_vpc} after the \
             detach, got {attaching:?}"
        );

        // Every other pair of the population still holds, read from the VPC's side too.
        for (index, vpc_id) in built.vpcs.iter().enumerate() {
            if index == row.target {
                continue;
            }
            let attaching = gateways_attaching(&router, case, vpc_id).await;
            assert_eq!(
                attaching,
                vec![built.gateways[index].clone()],
                "case {case:?}: expected {vpc_id} to still report {} attaching it, got \
                 {attaching:?}",
                built.gateways[index]
            );
        }
    }
}

/// Both sides of a detached pair are free to pair again: the gateway goes on to attach another
/// VPC, and the VPC goes on to take another gateway.
#[tokio::test]
async fn both_sides_of_a_detached_pair_pair_again() {
    for row in detach_cases() {
        // Pairing the freed sides with EACH OTHER'S partner needs a second pair, so the
        // single-pair rows are covered by the re-attach of the same pair below instead.
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let built = build(&router, case, &row.population).await;
        let target_gateway = built.gateways[row.target].clone();
        let target_vpc = built.vpcs[row.target].clone();

        detach(&router, case, &target_gateway, &target_vpc).await;

        // The freed VPC takes a gateway that was attached to nothing, when the population holds
        // one; otherwise the same gateway goes back on, which is the one-pair form of "the VPC
        // is free to take a gateway again".
        let next_gateway = built
            .gateways
            .get(row.population.attached_pairs)
            .cloned()
            .unwrap_or_else(|| target_gateway.clone());
        attach(&router, case, &next_gateway, &target_vpc).await;
        let attaching = gateways_attaching(&router, case, &target_vpc).await;
        assert_eq!(
            attaching,
            vec![next_gateway.clone()],
            "case {case:?}: expected {target_vpc} to report {next_gateway} attaching it after \
             pairing again, got {attaching:?}"
        );

        if next_gateway == target_gateway {
            continue;
        }

        // And the freed GATEWAY goes on to a VPC of its own, created after the detach so it is
        // a VPC the gateway was never paired with.
        let next_vpc = create_vpc(&router, "10.90.0.0/16").await;
        attach(&router, case, &target_gateway, &next_vpc).await;
        let attaching = gateways_attaching(&router, case, &next_vpc).await;
        assert_eq!(
            attaching,
            vec![target_gateway.clone()],
            "case {case:?}: expected {next_vpc} to report {target_gateway} attaching it, got \
             {attaching:?}"
        );
    }
}

/// A detach of a pair that is not attached is refused with `Gateway.NotAttached` naming the
/// pair the REQUEST sent, and removes nothing: neither a cross pair of two attachments that do
/// hold, nor the same pair a second time.
#[tokio::test]
async fn a_detach_of_a_pair_that_is_not_attached_is_refused() {
    let case = "three attached pairs, every pair of the cross product";
    let (router, _engine) = super::ec2_query::emulator();
    let built = build(
        &router,
        case,
        &Population {
            attached_pairs: 3,
            spare_gateways: 1,
        },
    )
    .await;
    let before = report(&router, case).await;

    // Every (gateway, VPC) pair of the population that is NOT one of the attachments: the
    // crossed pairs, and the unattached gateway against each VPC.
    for (gateway_index, internet_gateway_id) in built.gateways.iter().enumerate() {
        for (vpc_index, vpc_id) in built.vpcs.iter().enumerate() {
            if gateway_index == vpc_index {
                continue;
            }
            let pair = format!("{internet_gateway_id} and {vpc_id}");
            let (status, body) = detach_call(&router, internet_gateway_id, vpc_id).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 detaching the unattached pair {pair}, got \
                 {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                GATEWAY_NOT_ATTACHED,
                "case {case:?}: expected {GATEWAY_NOT_ATTACHED} detaching the unattached pair \
                 {pair}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                format!("resource {internet_gateway_id} is not attached to network {vpc_id}"),
                "case {case:?}: expected the refusal of {pair} to name the pair the request \
                 sent, got {body}"
            );
        }
    }

    // A second detach of a pair that WAS attached is the same refusal: the attachment is gone,
    // so there is nothing left to take.
    let internet_gateway_id = built.gateways[0].clone();
    let vpc_id = built.vpcs[0].clone();
    detach(&router, case, &internet_gateway_id, &vpc_id).await;
    let (status, body) = detach_call(&router, &internet_gateway_id, &vpc_id).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 detaching {internet_gateway_id} from {vpc_id} twice, \
         got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        GATEWAY_NOT_ATTACHED,
        "case {case:?}: expected {GATEWAY_NOT_ATTACHED} detaching {internet_gateway_id} from \
         {vpc_id} twice, got {body}"
    );

    // Only that one detach changed anything: every refused call left the report alone.
    let after = report(&router, case).await;
    let expected: Vec<ReportedGateway> = before
        .iter()
        .map(|gateway| {
            if gateway.internet_gateway_id == internet_gateway_id {
                ReportedGateway {
                    attachment_set_present: true,
                    attachments: Vec::new(),
                    ..gateway.clone()
                }
            } else {
                gateway.clone()
            }
        })
        .collect();
    assert_eq!(
        after, expected,
        "case {case:?}: expected the refused detaches to remove nothing, got {after:?}"
    );
}

/// One row of the unresolvable-id table: which side of the pair the request makes up, and the
/// refusal that side's resource answers with.
#[derive(Debug, Clone)]
struct UnknownIdCase {
    case: &'static str,
    /// The gateway id to send, or `None` to send the one the account really holds.
    internet_gateway_id: Option<&'static str>,
    /// The VPC id to send, or `None` to send the one the account really holds.
    vpc_id: Option<&'static str>,
    /// The codes the refusal may carry: one entry when exactly one side of the pair is
    /// unresolvable, both when neither is, since nothing in the model or the operation's doc
    /// fixes which side a request that makes up both is refused by.
    expected_codes: &'static [(&'static str, &'static str)],
}

/// A detach naming an id this account holds no resource for is refused with that resource's own
/// not-found code, worded with the id the REQUEST carried, and removes nothing.
#[tokio::test]
async fn a_detach_naming_an_unheld_id_is_refused_by_that_resource() {
    // Each code and its `The <kind> '<id>' does not exist` wording is EC2's for the member the
    // request made up: `InvalidInternetGatewayID.NotFound` for `$InternetGatewayId`,
    // `InvalidVpcID.NotFound` for `$VpcId`.
    let cases = [
        UnknownIdCase {
            case: "a gateway id of the right form that names no gateway",
            internet_gateway_id: Some("igw-0123456789abcdef0"),
            vpc_id: None,
            expected_codes: &[("InvalidInternetGatewayID.NotFound", "internetGateway ID")],
        },
        UnknownIdCase {
            case: "a short-form gateway id that names no gateway",
            internet_gateway_id: Some("igw-1a2b3c4d"),
            vpc_id: None,
            expected_codes: &[("InvalidInternetGatewayID.NotFound", "internetGateway ID")],
        },
        UnknownIdCase {
            case: "a VPC id of the right form that names no VPC",
            internet_gateway_id: None,
            vpc_id: Some("vpc-0123456789abcdef0"),
            expected_codes: &[("InvalidVpcID.NotFound", "vpc ID")],
        },
        UnknownIdCase {
            case: "a short-form VPC id that names no VPC",
            internet_gateway_id: None,
            vpc_id: Some("vpc-1a2b3c4d"),
            expected_codes: &[("InvalidVpcID.NotFound", "vpc ID")],
        },
        UnknownIdCase {
            // Which side such a request is refused by is not something the model or the
            // operation's doc states, so either not-found refusal satisfies the rule — what
            // must not happen is a detachment of a pair neither side of which exists.
            case: "neither side names a resource the account holds",
            internet_gateway_id: Some("igw-00000000000000000"),
            vpc_id: Some("vpc-00000000000000000"),
            expected_codes: &[
                ("InvalidInternetGatewayID.NotFound", "internetGateway ID"),
                ("InvalidVpcID.NotFound", "vpc ID"),
            ],
        },
    ];

    for row in cases {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let built = build(
            &router,
            case,
            &Population {
                attached_pairs: 1,
                spare_gateways: 0,
            },
        )
        .await;
        let held_gateway = built.gateways[0].clone();
        let held_vpc = built.vpcs[0].clone();
        let before = report(&router, case).await;

        let sent_gateway = row.internet_gateway_id.unwrap_or(&held_gateway).to_string();
        let sent_vpc = row.vpc_id.unwrap_or(&held_vpc).to_string();
        let (status, body) = detach_call(&router, &sent_gateway, &sent_vpc).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 detaching {sent_gateway} from {sent_vpc}, got \
             {status} with body {body}"
        );
        let code = error_code(&body);
        let matched = row
            .expected_codes
            .iter()
            .find(|(expected_code, _)| *expected_code == code);
        let Some((_, kind)) = matched else {
            panic!(
                "case {case:?}: expected one of {:?} detaching {sent_gateway} from {sent_vpc}, \
                 got {code} in {body}",
                row.expected_codes
            );
        };
        // The unresolvable id the refusal names is the one belonging to the resource whose code
        // it carried, and it is the id the REQUEST sent.
        let unheld = if *kind == "vpc ID" {
            sent_vpc.clone()
        } else {
            sent_gateway.clone()
        };
        assert_eq!(
            error_message(&body),
            format!("The {kind} '{unheld}' does not exist"),
            "case {case:?}: expected the refusal to name the id the request sent, got {body}"
        );

        let after = report(&router, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused detach to remove nothing, got {after:?}"
        );
    }
}

/// A subnet of `vpc_id` created over the wire, by id.
async fn create_subnet(router: &MultiServiceRouter, vpc_id: &str, cidr_block: &str) -> String {
    let params = [param("VpcId", vpc_id), param("CidrBlock", cidr_block)];
    let (status, body) = super::ec2_query::call(router, REGION, "CreateSubnet", &params).await;
    assert_eq!(
        status, 200,
        "CreateSubnet {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "subnetId").unwrap_or_else(|| panic!("CreateSubnet answered no subnetId: {body}"))
}

/// One instance launched into `subnet_id`, by id.
async fn launch_instance(router: &MultiServiceRouter, subnet_id: &str) -> String {
    let params = [
        // Any image of the catalog: which one is launched is nothing this rule turns on.
        param("ImageId", "ami-0123456789abcdef0"),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        param("SubnetId", subnet_id),
    ];
    let (status, body) = super::ec2_query::call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "RunInstances in {subnet_id}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("RunInstances answered no instanceId: {body}"))
}

/// Hold the instance `instance_id` still in `state`.
///
/// The transient lifecycle states cannot be held across a call through the operations that start
/// them — the clock moves a launched instance on whether or not anybody calls — so the state is
/// written into the store the way the sibling instance suites seed the states no sequence of
/// calls can hold still.
fn park_instance(engine: &engine::InMemoryEngine, instance_id: &str, state: InstanceStateName) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let instance = guard
        .instances
        .get_mut(instance_id)
        .unwrap_or_else(|| panic!("the store must hold the launched instance {instance_id}"));
    instance.state = state;
    instance.pending_transition = None;
}

/// Allocate one Elastic IP address and associate it with `instance_id`, so the instance reports
/// a public address (`Instance$PublicIpAddress`).
async fn associate_elastic_ip(router: &MultiServiceRouter, instance_id: &str) {
    let (status, body) = super::ec2_query::call(router, REGION, "AllocateAddress", &[]).await;
    assert_eq!(
        status, 200,
        "AllocateAddress: expected HTTP 200, got {status} with body {body}"
    );
    let allocation_id = element(&body, "allocationId")
        .unwrap_or_else(|| panic!("AllocateAddress answered no allocationId: {body}"));
    let params = [
        param("AllocationId", &allocation_id),
        param("InstanceId", instance_id),
    ];
    let (status, body) = super::ec2_query::call(router, REGION, "AssociateAddress", &params).await;
    assert_eq!(
        status, 200,
        "AssociateAddress {allocation_id} to {instance_id}: expected HTTP 200, got {status} with \
         body {body}"
    );
}

/// The dependency the operation's own doc names — "the VPC must not contain any running
/// instances with Elastic IP addresses or public IPv4 addresses" (`DetachInternetGateway`) — is
/// scoped to the VPC the REQUEST names: such an instance in one VPC blocks only that VPC's
/// detach, and leaves every other pair of the account detachable.
#[tokio::test]
async fn the_running_public_address_dependency_is_scoped_to_the_named_vpc() {
    let case = "two attached pairs, the second VPC holding a running instance with an address";
    let (router, engine) = super::ec2_query::emulator();
    let built = build(
        &router,
        case,
        &Population {
            attached_pairs: 2,
            spare_gateways: 0,
        },
    )
    .await;

    // One instance per VPC, so the two pairs differ only in whether THAT VPC's instance holds a
    // public address while it runs.
    for (index, vpc_id) in built.vpcs.iter().enumerate() {
        let subnet_id = create_subnet(&router, vpc_id, &format!("10.{}.1.0/24", 20 + index)).await;
        let instance_id = launch_instance(&router, &subnet_id).await;
        park_instance(&engine, &instance_id, InstanceStateName::Running);
        if index == 1 {
            associate_elastic_ip(&router, &instance_id).await;
        }
    }

    // The VPC whose running instance holds no public address detaches: the dependency is not an
    // account-wide one.
    detach(&router, case, &built.gateways[0], &built.vpcs[0]).await;
    let attaching = gateways_attaching(&router, case, &built.vpcs[0]).await;
    assert!(
        attaching.is_empty(),
        "case {case:?}: expected no gateway attaching {} after its detach, got {attaching:?}",
        built.vpcs[0]
    );

    // The VPC that holds one does not, and its attachment stays in place.
    let blocked_gateway = built.gateways[1].clone();
    let blocked_vpc = built.vpcs[1].clone();
    let (status, body) = detach_call(&router, &blocked_gateway, &blocked_vpc).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 detaching {blocked_gateway} from {blocked_vpc}, got \
         {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DependencyViolation",
        "case {case:?}: expected DependencyViolation detaching {blocked_gateway} from \
         {blocked_vpc}, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Network {blocked_vpc} has some mapped public address(es). Please unmap those public \
             address(es) before detaching the gateway."
        ),
        "case {case:?}: expected the refusal to name the network the request sent, got {body}"
    );
    let attaching = gateways_attaching(&router, case, &blocked_vpc).await;
    assert_eq!(
        attaching,
        vec![blocked_gateway.clone()],
        "case {case:?}: expected the refused detach to leave {blocked_gateway} attaching \
         {blocked_vpc}, got {attaching:?}"
    );

    // The dependency is the RUNNING instance holding the address, so once it is no longer
    // running the same detach the doc allows goes through.
    let instance_id = only_instance_in_vpc(&router, case, &blocked_vpc).await;
    park_instance(&engine, &instance_id, InstanceStateName::Stopped);
    detach(&router, case, &blocked_gateway, &blocked_vpc).await;
    let attaching = gateways_attaching(&router, case, &blocked_vpc).await;
    assert!(
        attaching.is_empty(),
        "case {case:?}: expected no gateway attaching {blocked_vpc} once no running instance \
         held an address, got {attaching:?}"
    );
}

/// The id of the one instance `vpc_id` holds, read back through `DescribeInstances` narrowed by
/// `vpc-id` so the expectation is the instance the account really holds.
async fn only_instance_in_vpc(router: &MultiServiceRouter, case: &str, vpc_id: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeInstances",
        &filters(&[("vpc-id", &[vpc_id])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reporting the instances of {vpc_id}, got {status} with \
         body {body}"
    );
    let reported = super::ec2_query::elements(&body, "instanceId");
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one instance in {vpc_id}, got {reported:?}"
    );
    reported[0].clone()
}
