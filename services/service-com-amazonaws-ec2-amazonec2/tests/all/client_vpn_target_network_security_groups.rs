// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ApplySecurityGroupsToClientVpnTargetNetwork`, driven over the wire
//! the service actually speaks: `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rules under test are:
//!
//! 1. THE GROUPS THE REQUEST NAMES REPLACE THE ONES IN FORCE: "this action replaces the
//!    existing security groups with the specified security groups", written onto both records
//!    that report them — the endpoint's own `SecurityGroupIds`, which
//!    `DescribeClientVpnEndpoints` reports, and the association in the VPC the request named,
//!    whose `SecurityGroups` `DescribeClientVpnTargetNetworks` reports.
//! 2. EVERY RESOURCE THE REQUEST NAMES IS RESOLVED: an endpoint the account does not hold, a
//!    VPC it does not hold, a VPC the endpoint has no association in, and a security group of
//!    another VPC are each refused with that resource's own code, and nothing is written.
//! 3. THE DRY RUN IS ABOUT THE CALLER: it is answered before anything the request names is
//!    read, so a probe naming an endpoint this account holds nothing for still answers
//!    `DryRunOperation`.
//!
//! The rows differ along what the rule generalises over — how many groups, which VPC, which
//! unresolvable id — so no row is the specification.

use std::collections::BTreeSet;

use ec2::ops::types::ClientVpnEndpoint;
use ec2::state::{
    ClientVpnTargetNetworkSpec as TargetNetworkSpec, ComAmazonawsEc2Amazonec2State as Ec2State,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The ec2Query action under test.
const ACTION: &str = "ApplySecurityGroupsToClientVpnTargetNetwork";

/// The HTTP status EC2 answers a `DryRun` request with.
const DRY_RUN_STATUS: u16 = 412;

/// A Client VPN endpoint id of the form EC2 issues that the account holds no endpoint for.
const UNKNOWN_ENDPOINT: &str = "cvpn-endpoint-00000000000000000";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl AsRef<str>) -> (String, String) {
    (key.to_string(), value.as_ref().to_string())
}

/// The fields one call carries beside the action.
type Fields = Vec<(String, String)>;

/// One row of the refusal table: what the case is, the fields the call carries, and the code
/// it must be refused with.
type Refusal = (&'static str, Fields, &'static str);

/// Create one VPC and answer its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr: &str) -> String {
    let (status, body) = call(router, REGION, "CreateVpc", &[p("CidrBlock", cidr)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVpc must succeed, got {status} with body {body}"
    );
    element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no vpc: {body}"))
}

/// Create one security group in `vpc_id` and answer its id.
async fn create_group(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    group_name: &str,
) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateSecurityGroup",
        &[
            p("GroupName", group_name),
            p("GroupDescription", "the groups a Client VPN association takes"),
            p("VpcId", vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSecurityGroup must succeed, got {status} with body {body}"
    );
    element(&body, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup reported no group: {body}"))
}

/// Write one Client VPN endpoint the way `CreateClientVpnEndpoint` would, and answer its id.
///
/// SEEDED rather than created over the wire because this suite is about the groups an
/// association carries rather than about what a create reads: the store's own writer is what
/// the create itself goes through.
fn seed_endpoint(engine: &engine::InMemoryEngine, description: &str, groups: &[String]) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_client_vpn_endpoint(ClientVpnEndpoint {
            description: Some(description.to_string()),
            client_cidr_block: Some("10.100.0.0/22".to_string()),
            security_group_ids: Some(groups.to_vec()),
            ..Default::default()
        })
        .expect("the store must take the Client VPN endpoint")
}

/// Associate a subnet of `vpc_id` with `endpoint_id`, the way
/// `AssociateClientVpnTargetNetwork` would, and answer the association's id.
fn seed_association(
    engine: &engine::InMemoryEngine,
    endpoint_id: &str,
    vpc_id: &str,
    groups: &[String],
) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_client_vpn_target_network(TargetNetworkSpec {
            client_vpn_endpoint_id: endpoint_id.to_string(),
            target_network_id: Some("subnet-00000000000000001".to_string()),
            vpc_id: Some(vpc_id.to_string()),
            security_groups: groups.to_vec(),
            ..Default::default()
        })
        .expect("the store must take the association")
        .association_id
        .expect("a stored association carries the id it was minted with")
}

/// The security groups `DescribeClientVpnTargetNetworks` reports for one endpoint, as a set.
async fn described_association_groups(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
) -> BTreeSet<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeClientVpnTargetNetworks",
        &[p("ClientVpnEndpointId", endpoint_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeClientVpnTargetNetworks must succeed, got {status} with body \
         {body}"
    );
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "associationId").is_some())
        .flat_map(|association| set_items(association, "securityGroups"))
        .filter_map(|item| item.text().map(str::to_string))
        .collect()
}

/// The security groups `DescribeClientVpnEndpoints` reports for one endpoint, as a set.
async fn described_endpoint_groups(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
) -> BTreeSet<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeClientVpnEndpoints",
        &[p("ClientVpnEndpointId.1", endpoint_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeClientVpnEndpoints must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "clientVpnEndpointId").is_some())
        .flat_map(|endpoint| set_items(endpoint, "securityGroupIdSet"))
        .filter_map(|item| item.text().map(str::to_string))
        .collect()
}

/// Rule: the groups the request names replace the ones in force, on both records that report
/// them — and a second call replaces them again rather than adding to them.
#[tokio::test]
async fn the_named_groups_replace_the_ones_in_force() {
    let case = "applying security groups";
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(case, &router, "10.50.0.0/16").await;
    let held = create_group(case, &router, &vpc_id, "held-by-the-association").await;
    let first = create_group(case, &router, &vpc_id, "first-applied").await;
    let second = create_group(case, &router, &vpc_id, "second-applied").await;
    let endpoint_id = seed_endpoint(&engine, "the endpoint under test", std::slice::from_ref(&held));
    seed_association(&engine, &endpoint_id, &vpc_id, std::slice::from_ref(&held));

    // The rows differ in HOW MANY groups one call applies, which is what "replaces" has to
    // hold over: one group in place of one, then two in place of that one, then back to one.
    for (label, applied) in [
        ("one group", vec![first.clone()]),
        ("two groups", vec![first.clone(), second.clone()]),
        ("one group again", vec![second.clone()]),
    ] {
        let mut params = vec![
            p("ClientVpnEndpointId", &endpoint_id),
            p("VpcId", &vpc_id),
        ];
        for (nth, group) in applied.iter().enumerate() {
            params.push(p(&format!("SecurityGroupId.{}", nth + 1), group));
        }
        let (status, body) = call(&router, REGION, ACTION, &params).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the apply must succeed, got {status} with body {body}"
        );
        let answered: BTreeSet<String> = super::ec2_query::elements(&body, "item")
            .into_iter()
            .collect();
        let wanted: BTreeSet<String> = applied.iter().cloned().collect();
        assert_eq!(
            answered, wanted,
            "case {label:?}: the answer must carry the groups it applied"
        );

        let on_the_association = described_association_groups(label, &router, &endpoint_id).await;
        assert_eq!(
            on_the_association, wanted,
            "case {label:?}: the association must report exactly the applied groups"
        );
        let on_the_endpoint = described_endpoint_groups(label, &router, &endpoint_id).await;
        assert_eq!(
            on_the_endpoint, wanted,
            "case {label:?}: the endpoint must report exactly the applied groups"
        );
        assert!(
            !on_the_association.contains(&held),
            "case {label:?}: the group that was in force must have been replaced, got \
             {on_the_association:?}"
        );
    }
}

/// Rule: every resource the request names is resolved against the store it lives in, and a
/// request naming one it cannot resolve applies nothing.
#[tokio::test]
async fn a_request_naming_a_resource_the_account_does_not_hold_is_refused() {
    let case = "apply refusals";
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(case, &router, "10.60.0.0/16").await;
    let other_vpc = create_vpc(case, &router, "10.61.0.0/16").await;
    let group = create_group(case, &router, &vpc_id, "in-the-right-vpc").await;
    let elsewhere = create_group(case, &router, &other_vpc, "in-another-vpc").await;
    let endpoint_id = seed_endpoint(&engine, "the endpoint under test", std::slice::from_ref(&group));
    seed_association(&engine, &endpoint_id, &vpc_id, std::slice::from_ref(&group));
    // Six DISTINCT groups, which is one more than an association may carry: the ceiling is
    // about how many groups govern the association, so naming one group six times is one
    // group rather than six.
    let mut too_many: Vec<String> = Vec::new();
    for nth in 0..6 {
        too_many.push(create_group(case, &router, &vpc_id, &format!("one-of-six-{nth}")).await);
    }

    let rows: Vec<Refusal> = vec![
        (
            "an endpoint the account holds none under",
            vec![
                p("ClientVpnEndpointId", UNKNOWN_ENDPOINT),
                p("VpcId", &vpc_id),
                p("SecurityGroupId.1", &group),
            ],
            "InvalidClientVpnEndpointId.NotFound",
        ),
        (
            "a VPC the account does not hold",
            vec![
                p("ClientVpnEndpointId", &endpoint_id),
                p("VpcId", "vpc-00000000000000000"),
                p("SecurityGroupId.1", &group),
            ],
            "InvalidVpcID.NotFound",
        ),
        (
            "a VPC the endpoint has no association in",
            vec![
                p("ClientVpnEndpointId", &endpoint_id),
                p("VpcId", &other_vpc),
                p("SecurityGroupId.1", &elsewhere),
            ],
            "InvalidClientVpnActiveAssociationNotFound",
        ),
        (
            "a security group of another VPC",
            vec![
                p("ClientVpnEndpointId", &endpoint_id),
                p("VpcId", &vpc_id),
                p("SecurityGroupId.1", &elsewhere),
            ],
            "InvalidGroup.NotFound",
        ),
        (
            "a security group the account does not hold",
            vec![
                p("ClientVpnEndpointId", &endpoint_id),
                p("VpcId", &vpc_id),
                p("SecurityGroupId.1", "sg-00000000000000000"),
            ],
            "InvalidGroup.NotFound",
        ),
        (
            "more groups than an association may carry",
            {
                let mut params = vec![
                    p("ClientVpnEndpointId", &endpoint_id),
                    p("VpcId", &vpc_id),
                ];
                for (nth, one) in too_many.iter().enumerate() {
                    params.push(p(&format!("SecurityGroupId.{}", nth + 1), one));
                }
                params
            },
            "InvalidParameterValue",
        ),
    ];

    for (label, params, code_wanted) in rows {
        let (status, body) = call(&router, REGION, ACTION, &params).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {label:?}: expected {code_wanted}, got {code:?} in {body}"
        );
        let on_the_association = described_association_groups(label, &router, &endpoint_id).await;
        assert_eq!(
            on_the_association,
            [group.clone()].into_iter().collect::<BTreeSet<String>>(),
            "case {label:?}: a refused call must apply nothing, got {on_the_association:?}"
        );
    }
}

/// Rule: the dry run is a question about the CALLER, answered before anything the request
/// names is read — so a probe naming an endpoint this account holds nothing for still answers
/// `DryRunOperation`, and nothing is applied.
#[tokio::test]
async fn a_dry_run_is_answered_before_the_request_is_read() {
    let case = "dry run";
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(case, &router, "10.70.0.0/16").await;
    let group = create_group(case, &router, &vpc_id, "never-applied").await;
    let held = create_group(case, &router, &vpc_id, "held").await;
    let endpoint_id = seed_endpoint(&engine, "the endpoint under test", std::slice::from_ref(&held));
    seed_association(&engine, &endpoint_id, &vpc_id, std::slice::from_ref(&held));

    for (label, endpoint) in [
        ("naming the endpoint it holds", endpoint_id.clone()),
        ("naming an endpoint it does not hold", UNKNOWN_ENDPOINT.to_string()),
    ] {
        let (status, body) = call(
            &router,
            REGION,
            ACTION,
            &[
                p("ClientVpnEndpointId", &endpoint),
                p("VpcId", &vpc_id),
                p("SecurityGroupId.1", &group),
                p("DryRun", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "case {label:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "DryRunOperation",
            "case {label:?}: expected DryRunOperation, got {code:?} in {body}"
        );
    }

    let on_the_association = described_association_groups(case, &router, &endpoint_id).await;
    assert_eq!(
        on_the_association,
        [held].into_iter().collect::<BTreeSet<String>>(),
        "case {case:?}: a dry run must apply nothing, got {on_the_association:?}"
    );
}
