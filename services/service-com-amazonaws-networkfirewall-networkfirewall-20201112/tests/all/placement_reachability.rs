// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the placement a firewall is created in: the VPC it filters in and the
//! subnets its endpoints sit in.
//!
//! RULE under test: a firewall can only be placed in a VPC and in subnets the calling account
//! holds, so a request naming a VPC the account cannot reach, or naming any subnet it cannot
//! reach, is refused with HTTP 400 `InvalidRequestException` and creates or changes nothing —
//! the VPC answered first, a subnet being a resource of the VPC it sits in. The same holds for
//! a subnet an association would add to a firewall that already exists.
//!
//! Every test mounts Amazon EC2 beside Network Firewall on one engine, exactly as the
//! assembled emulator mounts them, because a VPC and a subnet are ITS resources: what this
//! account can reach is EC2's answer, and the reachable ids below are the ids EC2 minted.
//! Network Firewall is driven the way a client drives it, over the wire through the same
//! router the emulator boots.
//!
//! The tests are tables: each row is one input in the class the rule covers, and every
//! assertion names its row and the actual value.

use serde_json::{json, Value};

use crate::harness::{error_code, error_message, string_at, Harness, NOT_FOUND_MESSAGE};

/// The modelled error a request naming a resource it may not reference is refused with
/// (`smithy_query(query="shape",shape="InvalidRequestException")`: client error, HTTP 400 —
/// "your request references an ARN that is malformed, or corresponds to a resource that isn't
/// valid in the context of the request").
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The error a read or a delete naming a firewall this service does not hold is refused with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// What a refusal of a VPC this account cannot reach reads, verbatim: fixed text for this
/// error class, carrying nothing from the request.
const VPC_INACCESSIBLE: &str = "Given VPC is inaccessible from account";

/// What a refusal of subnets this account cannot reach reads, verbatim — one sentence for one
/// subnet or several, again carrying nothing from the request.
const SUBNETS_INACCESSIBLE: &str = "Given subnet(s) is/are inaccessible from account";

/// VPC ids of the form the `VpcId` pattern `^vpc-[0-9a-f]+$` admits that nothing in this
/// emulator ever minted — two spellings, so no row depends on one id's shape.
const UNREACHABLE_VPCS: [&str; 2] = ["vpc-0000000000bad0bad", "vpc-badf00d"];

/// Subnet ids of the form the `AzSubnet` pattern `^subnet-[0-9a-f]+$` admits that nothing in
/// this emulator ever minted, in two spellings for the same reason.
const UNREACHABLE_SUBNETS: [&str; 2] = ["subnet-0000000000bad0bad", "subnet-1dea"];

/// One VPC of the account with one subnet per entry of `subnet_cidrs`, created through Amazon
/// EC2's own API; every id is the one EC2 MINTED.
///
/// Each suite of this target that needs networks creates its own through this same API — this
/// one needs no route table, so it asks for none.
async fn reachable_network(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    vpc_cidr: &str,
    subnet_cidrs: &[&str],
) -> (String, Vec<String>) {
    let created = ec2
        .create_vpc()
        .cidr_block(vpc_cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create {vpc_cidr}: {error:?}"));
    let vpc_id = created
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must report the VPC it minted"))
        .to_string();

    let mut subnet_ids = Vec::with_capacity(subnet_cidrs.len());
    for cidr in subnet_cidrs {
        let created = ec2
            .create_subnet()
            .vpc_id(&vpc_id)
            .cidr_block(*cidr)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: EC2 must create the subnet {cidr}: {error:?}")
            });
        subnet_ids.push(
            created
                .subnet()
                .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: CreateSubnet must report the subnet it minted")
                })
                .to_string(),
        );
    }
    (vpc_id, subnet_ids)
}

/// Ask `CreateFirewall` for a firewall named `name` on `subnet_ids` of `vpc_id`, through a
/// policy created for it, and answer the raw response (status and body).
async fn create_firewall(
    harness: &Harness,
    name: &str,
    vpc_id: &str,
    subnet_ids: &[String],
) -> (u16, Value) {
    let policy_arn = harness.shared_policy(&format!("{name}-policy")).await;
    let mappings: Vec<Value> = subnet_ids
        .iter()
        .map(|subnet_id| json!({ "SubnetId": subnet_id }))
        .collect();
    harness
        .call(
            "CreateFirewall",
            &json!({
                "FirewallName": name,
                "FirewallPolicyArn": policy_arn,
                "VpcId": vpc_id,
                "SubnetMappings": mappings,
            }),
        )
        .await
}

/// The firewall names `ListFirewalls` reports.
async fn listed_firewalls(harness: &Harness) -> Vec<String> {
    let listed = harness.ok("ListFirewalls", &json!({})).await;
    listed["Firewalls"]
        .as_array()
        .unwrap_or_else(|| panic!("expected Firewalls in {listed:?}"))
        .iter()
        .map(|entry| string_at(entry, &["FirewallName"]).to_string())
        .collect()
}

/// The VPC endpoint ids Amazon EC2 holds in `vpc_id`.
async fn endpoints_in_vpc(ec2: &::aws_sdk_ec2::Client, case: &str, vpc_id: &str) -> Vec<String> {
    let by_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name("vpc-id")
        .values(vpc_id.to_string())
        .build();
    let described = ec2
        .describe_vpc_endpoints()
        .filters(by_vpc)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must describe the endpoints of {vpc_id}: {error:?}")
        });
    described
        .vpc_endpoints()
        .iter()
        .map(|endpoint| {
            endpoint
                .vpc_endpoint_id()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// RULE: `CreateFirewall` refuses a placement this account cannot reach — the VPC answered
/// first, then the subnets — with HTTP 400 `InvalidRequestException` and the fixed text of
/// that refusal, and the placement it CAN reach is served.
///
/// The table spans which part of the placement the account cannot reach and how many entries
/// are unreachable: a VPC alone, a VPC whose subnets are unreachable too (which the VPC's own
/// refusal still covers), one unreachable subnet, one unreachable subnet beside a reachable
/// one, two unreachable subnets, and — the control — a placement that is entirely the
/// account's own.
#[tokio::test]
async fn create_refuses_a_placement_the_account_cannot_reach() {
    /// Which ids a row puts in the request.
    enum Names {
        /// An unreachable VPC, with the account's own subnets.
        UnreachableVpc,
        /// An unreachable VPC and unreachable subnets.
        UnreachableBoth,
        /// The account's own VPC, with one unreachable subnet.
        UnreachableSubnet,
        /// The account's own VPC and one of its subnets, plus one unreachable subnet.
        OwnThenUnreachableSubnet,
        /// The account's own VPC with two unreachable subnets.
        TwoUnreachableSubnets,
        /// Every id the account's own.
        Reachable,
    }
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidrs: &'static [&'static str],
        names: Names,
        /// The message the create must be refused with, or `None` when it must be served.
        refused_with: Option<&'static str>,
    }
    let cases = [
        Case {
            name: "unreachable-vpc",
            vpc_cidr: "10.41.0.0/16",
            subnet_cidrs: &["10.41.1.0/24"],
            names: Names::UnreachableVpc,
            refused_with: Some(VPC_INACCESSIBLE),
        },
        Case {
            name: "unreachable-vpc-and-subnets",
            vpc_cidr: "10.42.0.0/16",
            subnet_cidrs: &["10.42.1.0/24"],
            names: Names::UnreachableBoth,
            refused_with: Some(VPC_INACCESSIBLE),
        },
        Case {
            name: "unreachable-subnet",
            vpc_cidr: "10.43.0.0/16",
            subnet_cidrs: &["10.43.1.0/24"],
            names: Names::UnreachableSubnet,
            refused_with: Some(SUBNETS_INACCESSIBLE),
        },
        Case {
            name: "own-then-unreachable-subnet",
            vpc_cidr: "10.44.0.0/16",
            subnet_cidrs: &["10.44.1.0/24"],
            names: Names::OwnThenUnreachableSubnet,
            refused_with: Some(SUBNETS_INACCESSIBLE),
        },
        Case {
            name: "two-unreachable-subnets",
            vpc_cidr: "10.45.0.0/16",
            subnet_cidrs: &["10.45.1.0/24"],
            names: Names::TwoUnreachableSubnets,
            refused_with: Some(SUBNETS_INACCESSIBLE),
        },
        Case {
            name: "reachable-placement",
            vpc_cidr: "10.46.0.0/16",
            subnet_cidrs: &["10.46.1.0/24", "10.46.2.0/24"],
            names: Names::Reachable,
            refused_with: None,
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let (own_vpc, own_subnets) =
            reachable_network(&ec2, case.name, case.vpc_cidr, case.subnet_cidrs).await;

        let (vpc_id, subnet_ids): (String, Vec<String>) = match case.names {
            Names::UnreachableVpc => (UNREACHABLE_VPCS[0].to_string(), own_subnets.clone()),
            Names::UnreachableBoth => (
                UNREACHABLE_VPCS[1].to_string(),
                vec![UNREACHABLE_SUBNETS[0].to_string()],
            ),
            Names::UnreachableSubnet => (
                own_vpc.clone(),
                vec![UNREACHABLE_SUBNETS[1].to_string()],
            ),
            Names::OwnThenUnreachableSubnet => (
                own_vpc.clone(),
                vec![own_subnets[0].clone(), UNREACHABLE_SUBNETS[0].to_string()],
            ),
            Names::TwoUnreachableSubnets => (
                own_vpc.clone(),
                UNREACHABLE_SUBNETS.iter().map(|id| id.to_string()).collect(),
            ),
            Names::Reachable => (own_vpc.clone(), own_subnets.clone()),
        };

        let (status, body) = create_firewall(&harness, case.name, &vpc_id, &subnet_ids).await;
        let Some(expected_message) = case.refused_with else {
            assert_eq!(
                status, 200,
                "case {name:?}: expected the account's own placement to be served, got HTTP \
                 {status} with {body:?}",
                name = case.name
            );
            assert_eq!(
                listed_firewalls(&harness).await,
                vec![case.name.to_string()],
                "case {name:?}: expected the served create to leave exactly its firewall",
                name = case.name
            );
            continue;
        };

        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body).as_str()),
            (400, INVALID_REQUEST, expected_message),
            "case {name:?}: expected the unreachable placement to be refused, got HTTP {status} \
             with {body:?}",
            name = case.name
        );

        // Nothing was created: the name resolves nowhere, for a read, for a delete and in the
        // listing, and no firewall endpoint was left behind in the account's own VPC.
        let (read_status, read) = harness
            .call("DescribeFirewall", &json!({ "FirewallName": case.name }))
            .await;
        assert_eq!(
            (read_status, error_code(&read).as_str(), error_message(&read).as_str()),
            (400, NOT_FOUND, NOT_FOUND_MESSAGE),
            "case {name:?}: expected no firewall to have been created, the read answered HTTP \
             {read_status} with {read:?}",
            name = case.name
        );
        let (delete_status, deleted) = harness
            .call("DeleteFirewall", &json!({ "FirewallName": case.name }))
            .await;
        assert_eq!(
            (delete_status, error_code(&deleted).as_str(), error_message(&deleted).as_str()),
            (400, NOT_FOUND, NOT_FOUND_MESSAGE),
            "case {name:?}: expected the delete to find nothing, it answered HTTP \
             {delete_status} with {deleted:?}",
            name = case.name
        );
        assert_eq!(
            listed_firewalls(&harness).await,
            Vec::<String>::new(),
            "case {name:?}: expected the refused create to leave no firewall listed",
            name = case.name
        );
        let held = endpoints_in_vpc(&ec2, case.name, &own_vpc).await;
        assert_eq!(
            held,
            Vec::<String>::new(),
            "case {name:?}: expected the refusal to instantiate no endpoint in {own_vpc}, EC2 \
             holds {held:?}",
            name = case.name
        );
    }
}

/// RULE (the same rule on the operation that grows a firewall's placement): `AssociateSubnets`
/// refuses a subnet this account cannot reach with the same HTTP 400
/// `InvalidRequestException`, leaves the firewall's mapping set exactly as it was, and serves
/// the identical call for a subnet the account does hold.
#[tokio::test]
async fn association_refuses_a_subnet_the_account_cannot_reach() {
    struct Case {
        name: &'static str,
        /// The subnets the association names beside the firewall's own first subnet.
        unreachable: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "one-unreachable",
            unreachable: &[UNREACHABLE_SUBNETS[0]],
        },
        Case {
            name: "two-unreachable",
            unreachable: &UNREACHABLE_SUBNETS,
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let (vpc_id, own_subnets) = reachable_network(
            &ec2,
            case.name,
            "10.47.0.0/16",
            &["10.47.1.0/24", "10.47.2.0/24"],
        )
        .await;
        let (status, created) =
            create_firewall(&harness, case.name, &vpc_id, &own_subnets[..1]).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the firewall to be created, got HTTP {status} with \
             {created:?}",
            name = case.name
        );

        let mappings: Vec<Value> = case
            .unreachable
            .iter()
            .map(|subnet_id| json!({ "SubnetId": subnet_id }))
            .collect();
        let (status, body) = harness
            .call(
                "AssociateSubnets",
                &json!({ "FirewallName": case.name, "SubnetMappings": mappings }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body).as_str()),
            (400, INVALID_REQUEST, SUBNETS_INACCESSIBLE),
            "case {name:?}: expected the unreachable subnets to be refused, got HTTP {status} \
             with {body:?}",
            name = case.name
        );
        let described = harness
            .ok("DescribeFirewall", &json!({ "FirewallName": case.name }))
            .await;
        let mapped: Vec<String> = described["Firewall"]["SubnetMappings"]
            .as_array()
            .unwrap_or_else(|| panic!("case {name:?}: expected SubnetMappings in {described:?}", name = case.name))
            .iter()
            .map(|mapping| string_at(mapping, &["SubnetId"]).to_string())
            .collect();
        assert_eq!(
            mapped,
            vec![own_subnets[0].clone()],
            "case {name:?}: expected the refusal to leave the mapping set untouched",
            name = case.name
        );

        // The positive control: the identical call for a subnet the account holds is served.
        let served = harness
            .ok(
                "AssociateSubnets",
                &json!({
                    "FirewallName": case.name,
                    "SubnetMappings": [{ "SubnetId": own_subnets[1] }],
                }),
            )
            .await;
        let grown: Vec<String> = served["SubnetMappings"]
            .as_array()
            .unwrap_or_else(|| panic!("case {name:?}: expected SubnetMappings in {served:?}", name = case.name))
            .iter()
            .map(|mapping| string_at(mapping, &["SubnetId"]).to_string())
            .collect();
        assert_eq!(
            grown,
            vec![own_subnets[0].clone(), own_subnets[1].clone()],
            "case {name:?}: expected the reachable subnet to be associated",
            name = case.name
        );
    }
}

/// RULE (the bound of the same rule): what this account can reach is Amazon EC2's answer, so
/// where there is no EC2 in the process to ask, the placement resolves neither way and the
/// create is refused for nothing — the refusals above are grounded in what EC2 ANSWERED, and
/// an ask that went unanswered grounds none.
#[tokio::test]
async fn an_unanswered_placement_refuses_nothing() {
    struct Case {
        name: &'static str,
        vpc_id: &'static str,
        subnet_ids: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "unanswered-vpc",
            vpc_id: UNREACHABLE_VPCS[0],
            subnet_ids: &[UNREACHABLE_SUBNETS[0]],
        },
        Case {
            name: "unanswered-subnets",
            vpc_id: UNREACHABLE_VPCS[1],
            subnet_ids: &UNREACHABLE_SUBNETS,
        },
    ];

    for case in cases {
        // No EC2 mounted beside this service, which is what makes the ask unanswerable.
        let harness = Harness::new();
        let subnet_ids: Vec<String> = case.subnet_ids.iter().map(|id| id.to_string()).collect();
        let (status, body) = create_firewall(&harness, case.name, case.vpc_id, &subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the create to be served where no EC2 answers, got HTTP \
             {status} with {body:?}",
            name = case.name
        );
        assert_eq!(
            string_at(&body, &["Firewall", "VpcId"]),
            case.vpc_id,
            "case {name:?}: expected the firewall to report the VPC the request named, got \
             {body:?}",
            name = case.name
        );
    }
}
