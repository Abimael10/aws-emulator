// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests pinning HOW TOTAL the at-least-one-subnet-mapping refusal is:
//! a `DisassociateSubnets` that would empty a firewall's mapping set is refused
//! however the request names the firewall and whether or not it carries the
//! optimistic-locking token, and the refusal writes nothing anywhere — neither in
//! this service's record nor in Amazon EC2, which HOLDS the firewall endpoints
//! those mappings are.
//!
//! The mapping-count and ordering dimensions of the same rule live in
//! `subnet_mapping_minimum`; these rows vary the dimensions that one leaves
//! fixed — the identification form (`FirewallName`, `FirewallArn`, both), the
//! presence of a current `UpdateToken`, repeating the refused call, and the
//! endpoint the other component holds.
//!
//! Every test drives the service the way a client does, through the shared
//! [`Harness`]: an awsJson1_0 `POST /` with
//! `X-Amz-Target: NetworkFirewall_20201112.<OperationName>`, so what is asserted
//! is the WIRE behaviour and not a handler's return value.
//!
//! Every subnet id written out below satisfies the member pattern the model puts
//! on the ids these operations take
//! (`smithy_query(query="shape",shape="AzSubnets")`: `^subnet-[0-9a-f]+$`), so a
//! row exercises the rule rather than that constraint; the rows that need a
//! subnet Amazon EC2 holds use the ids EC2 minted.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::harness::{error_code, error_message, string_at, Harness};

/// How long a test waits for a CONDITION the clock owes before giving up. Far
/// above the firewall's own provisioning span, because what the wait below loops
/// on is the state the emulator reports, never an elapsed time.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The fixed sentence the at-least-one-mapping refusal carries, ahead of the
/// subnets the request asked to remove.
const LAST_MAPPING_STEM: &str = "availability zone subnet(s) cannot be removed as firewall must \
                                 have at least one subnet mapping";

/// Create a firewall named `name` in `vpc_id` carrying `subnets`.
async fn create_firewall(
    harness: &Harness,
    name: &str,
    policy_arn: &str,
    vpc_id: &str,
    subnets: &[String],
) {
    harness
        .ok(
            "CreateFirewall",
            &json!({
                "FirewallName": name,
                "FirewallPolicyArn": policy_arn,
                "VpcId": vpc_id,
                "SubnetMappings": subnets
                    .iter()
                    .map(|id| json!({ "SubnetId": id }))
                    .collect::<Vec<Value>>(),
            }),
        )
        .await;
}

/// Everything a later assertion compares a firewall against: the mapping set it
/// reports, its `UpdateToken`, and the `(Availability Zone, SubnetId, EndpointId)`
/// of every attachment it carries.
#[derive(Debug, PartialEq, Eq)]
struct Reported {
    subnet_ids: Vec<String>,
    update_token: String,
    attachments: BTreeSet<(String, String, String)>,
}

/// Read `name` back through `DescribeFirewall`.
async fn report(harness: &Harness, name: &str) -> Reported {
    let described = harness
        .ok("DescribeFirewall", &json!({ "FirewallName": name }))
        .await;
    let subnet_ids = described["Firewall"]["SubnetMappings"]
        .as_array()
        .unwrap_or_else(|| panic!("expected Firewall.SubnetMappings in {described:?}"))
        .iter()
        .map(|mapping| string_at(mapping, &["SubnetId"]).to_string())
        .collect();
    let attachments = described["FirewallStatus"]["SyncStates"]
        .as_object()
        .unwrap_or_else(|| panic!("expected FirewallStatus.SyncStates in {described:?}"))
        .iter()
        .map(|(zone, sync_state)| {
            (
                zone.clone(),
                string_at(sync_state, &["Attachment", "SubnetId"]).to_string(),
                string_at(sync_state, &["Attachment", "EndpointId"]).to_string(),
            )
        })
        .collect();
    Reported {
        subnet_ids,
        update_token: string_at(&described, &["UpdateToken"]).to_string(),
        attachments,
    }
}

/// The ARN `DescribeFirewall` reports for `name`.
async fn firewall_arn(harness: &Harness, name: &str) -> String {
    let described = harness
        .ok("DescribeFirewall", &json!({ "FirewallName": name }))
        .await;
    string_at(&described, &["Firewall", "FirewallArn"]).to_string()
}

/// How a row names the firewall the refused call acts on. The model documents
/// every one of these forms on the input ("You must specify the ARN or the name,
/// and you can specify both").
#[derive(Debug, Clone, Copy)]
enum Identify {
    Name,
    Arn,
    NameAndArn,
}

/// One row: the mappings the firewall carries, how the call names it, and whether
/// the call carries the firewall's CURRENT `UpdateToken`.
#[derive(Debug)]
struct Case {
    /// The row's label, which is also the firewall's name.
    label: &'static str,
    initial: &'static [&'static str],
    identify: Identify,
    with_token: bool,
}

/// The `DisassociateSubnets` body a row sends: its whole mapping set, named the
/// row's way.
fn disassociate_request(case: &Case, arn: &str, token: &str) -> Value {
    let mut body = Map::new();
    match case.identify {
        Identify::Name => {
            body.insert("FirewallName".to_string(), json!(case.label));
        }
        Identify::Arn => {
            body.insert("FirewallArn".to_string(), json!(arn));
        }
        Identify::NameAndArn => {
            body.insert("FirewallName".to_string(), json!(case.label));
            body.insert("FirewallArn".to_string(), json!(arn));
        }
    }
    if case.with_token {
        body.insert("UpdateToken".to_string(), json!(token));
    }
    body.insert("SubnetIds".to_string(), json!(case.initial));
    Value::Object(body)
}

/// RULE: a firewall filters through the endpoints its subnet mappings are, so
/// `DisassociateSubnets` never leaves it with none — a request naming every
/// mapping the firewall still has is refused with HTTP 400 /
/// `InvalidRequestException` naming those subnets, and nothing is written — and
/// that refusal depends on the surviving mapping set alone: not on how the
/// request identifies the firewall, not on whether it carries the current
/// optimistic-locking token, and not on how often it is repeated.
#[tokio::test]
async fn the_last_mapping_refusal_depends_on_the_surviving_set_alone() {
    let harness = Harness::new();
    let policy_arn = harness.shared_policy("last-mapping-scope-policy").await;

    let cases = [
        Case {
            label: "scope-by-name",
            initial: &["subnet-a0"],
            identify: Identify::Name,
            with_token: false,
        },
        Case {
            label: "scope-by-arn",
            initial: &["subnet-b0"],
            identify: Identify::Arn,
            with_token: false,
        },
        Case {
            label: "scope-by-arn-with-token",
            initial: &["subnet-c0"],
            identify: Identify::Arn,
            with_token: true,
        },
        Case {
            label: "scope-by-both-two-mappings",
            initial: &["subnet-d0", "subnet-d1"],
            identify: Identify::NameAndArn,
            with_token: true,
        },
        Case {
            label: "scope-by-name-with-token-three",
            initial: &["subnet-e0", "subnet-e1", "subnet-e2"],
            identify: Identify::Name,
            with_token: true,
        },
    ];

    for case in &cases {
        let subnets: Vec<String> = case.initial.iter().map(|id| (*id).to_string()).collect();
        create_firewall(&harness, case.label, &policy_arn, "vpc-0123abcd", &subnets).await;
        let arn = firewall_arn(&harness, case.label).await;
        let before = report(&harness, case.label).await;
        let request = disassociate_request(case, &arn, &before.update_token);
        let expected_message = format!(
            "{LAST_MAPPING_STEM}, parameter: [{}]",
            case.initial.join(", ")
        );

        // Repeating the refused call changes neither the answer nor the resource:
        // a refusal consumes nothing.
        for attempt in 1..=2 {
            let (status, body) = harness.call("DisassociateSubnets", &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} attempt {attempt}: expected HTTP 400, got {status} with {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidRequestException",
                "case {case:?} attempt {attempt}: expected InvalidRequestException, got {body:?}"
            );
            assert_eq!(
                error_message(&body),
                expected_message,
                "case {case:?} attempt {attempt}: expected the at-least-one-mapping refusal \
                 naming the request's subnets, got {body:?}"
            );

            let after = report(&harness, case.label).await;
            assert_eq!(
                after.subnet_ids, before.subnet_ids,
                "case {case:?} attempt {attempt}: expected the mapping set to be left alone, got \
                 {after:?}"
            );
            assert_eq!(
                after.update_token, before.update_token,
                "case {case:?} attempt {attempt}: expected the UpdateToken to be left alone, got \
                 {after:?}"
            );
            assert_eq!(
                after.attachments, before.attachments,
                "case {case:?} attempt {attempt}: expected every attachment to be left alone, got \
                 {after:?}"
            );
        }
    }
}

/// Poll `DescribeFirewall` until the firewall reports `READY`.
///
/// Waits for the CONDITION rather than for a duration: the clock owes the
/// firewall its `PROVISIONING` -> `READY` move, so a loaded host only makes this
/// loop more times.
async fn ready_firewall(harness: &Harness, case: &str, name: &str) {
    let deadline = Instant::now() + CONDITION_DEADLINE;
    loop {
        let described = harness
            .ok("DescribeFirewall", &json!({ "FirewallName": name }))
            .await;
        if described["FirewallStatus"]["Status"] == json!("READY") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the firewall {name} to reach READY, last saw {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ids of the VPC endpoints Amazon EC2 holds in `vpc_id`.
async fn endpoint_ids_in_vpc(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    vpc_id: &str,
) -> BTreeSet<String> {
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
            panic!("case {case:?}: EC2 must report the endpoints of {vpc_id}: {error:?}")
        });
    described
        .vpc_endpoints()
        .iter()
        .map(|endpoint| endpoint.vpc_endpoint_id().unwrap_or_default().to_string())
        .collect()
}

/// Create a VPC with one subnet per entry of `subnet_cidrs` through Amazon EC2's
/// own API, and answer `(vpc id, subnet ids)` — every id the one EC2 minted.
async fn create_network(
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

/// RULE (the other component's side of the same invariant): the refused call
/// removes no firewall endpoint either — Amazon EC2, which holds the Gateway Load
/// Balancer endpoint each mapping is, still holds it after the refusal — while the
/// same call becomes servable the moment the firewall has another mapping to
/// survive on, and then deletes exactly that one subnet's endpoint.
#[tokio::test]
async fn the_last_mapping_refusal_deletes_no_endpoint_in_ec2() {
    let case = "last-mapping-endpoint";
    let harness = Harness::with_ec2();
    let ec2 = harness.ec2();
    let (vpc_id, subnet_ids) =
        create_network(&ec2, case, "10.51.0.0/16", &["10.51.1.0/24", "10.51.2.0/24"]).await;
    let policy_arn = harness.shared_policy("last-mapping-endpoint-policy").await;
    create_firewall(
        &harness,
        "endpoint-fw",
        &policy_arn,
        &vpc_id,
        &subnet_ids[..1],
    )
    .await;
    ready_firewall(&harness, case, "endpoint-fw").await;

    let before = report(&harness, "endpoint-fw").await;
    let held_before = endpoint_ids_in_vpc(&ec2, case, &vpc_id).await;
    assert_eq!(
        held_before.len(),
        1,
        "case {case:?}: expected EC2 to hold the firewall's one endpoint, it holds {held_before:?}"
    );

    let (status, body) = harness
        .call(
            "DisassociateSubnets",
            &json!({ "FirewallName": "endpoint-fw", "SubnetIds": [subnet_ids[0]] }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the last mapping's removal to be refused, got {status} with \
         {body:?}"
    );
    assert_eq!(
        error_message(&body),
        format!("{LAST_MAPPING_STEM}, parameter: [{}]", subnet_ids[0]),
        "case {case:?}: expected the at-least-one-mapping refusal naming the request's subnet, \
         got {body:?}"
    );
    let after = report(&harness, "endpoint-fw").await;
    assert_eq!(
        after, before,
        "case {case:?}: expected the refusal to leave the firewall exactly as it was"
    );
    assert_eq!(
        endpoint_ids_in_vpc(&ec2, case, &vpc_id).await,
        held_before,
        "case {case:?}: expected the refusal to delete no endpoint in Amazon EC2"
    );

    // The positive control: with a second mapping to survive on, the identical
    // call is served, and exactly the named subnet's endpoint goes.
    harness
        .ok(
            "AssociateSubnets",
            &json!({
                "FirewallName": "endpoint-fw",
                "SubnetMappings": [{ "SubnetId": subnet_ids[1] }],
            }),
        )
        .await;
    ready_firewall(&harness, case, "endpoint-fw").await;
    let grown = report(&harness, "endpoint-fw").await;
    let grown_endpoints = endpoint_ids_in_vpc(&ec2, case, &vpc_id).await;
    assert_eq!(
        grown_endpoints.len(),
        2,
        "case {case:?}: expected EC2 to hold both endpoints, it holds {grown_endpoints:?}"
    );

    let served = harness
        .ok(
            "DisassociateSubnets",
            &json!({ "FirewallName": "endpoint-fw", "SubnetIds": [subnet_ids[0]] }),
        )
        .await;
    let reported: Vec<String> = served["SubnetMappings"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected SubnetMappings in {served:?}"))
        .iter()
        .map(|mapping| string_at(mapping, &["SubnetId"]).to_string())
        .collect();
    assert_eq!(
        reported,
        vec![subnet_ids[1].clone()],
        "case {case:?}: expected only the second subnet to remain, got {reported:?}"
    );
    let dropped: BTreeSet<String> = grown
        .attachments
        .iter()
        .filter(|(_, subnet_id, _)| subnet_id == &subnet_ids[0])
        .map(|(_, _, endpoint_id)| endpoint_id.clone())
        .collect();
    assert_eq!(
        endpoint_ids_in_vpc(&ec2, case, &vpc_id).await,
        grown_endpoints.difference(&dropped).cloned().collect(),
        "case {case:?}: expected exactly the disassociated subnet's endpoint {dropped:?} to be \
         deleted in Amazon EC2"
    );
}
