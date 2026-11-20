// SPDX-License-Identifier: CC0-1.0

//! The response elements the live EC2 wire renders that this checkout's Smithy model does
//! not declare, plus the two state facts the same reads depend on — driven over the wire
//! the service actually speaks (`ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness).
//!
//! The rules under test:
//!
//! 1. A REPORTED SHAPE CARRIES THE ELEMENTS THE LIVE WIRE RENDERS ON IT, whichever
//!    operation reports the shape and however many of the shape one response carries —
//!    `dhcpOptionsSource` on a VPC, naming the scope that supplied its DHCP option set, and
//!    `verifiedAccessInstanceTransitGatewayAttachmentSet` on a Verified Access instance,
//!    present and empty for an instance holding no transit-gateway attachment. Neither is
//!    in the model's member list for its shape (`com.amazonaws.ec2#Vpc`,
//!    `com.amazonaws.ec2#VerifiedAccessInstance`), so the recording is the authority for
//!    both; each is still DERIVED from the record reported, which rule 2 pins by taking the
//!    derivation away.
//! 2. THE SCOPE IS READ OFF THE RECORD: a VPC the store holds no DHCP option set
//!    association for reports no `dhcpOptionsSource`, there being no scope to name.
//! 3. A VPC IS `available` ONCE THE CREATE THAT MADE IT HAS RETURNED: the create answers
//!    the transient `pending` the VPC was minted in, and every call after it reports
//!    `available` — a VPC is provisioned inside the call that creates it.
//! 4. A POOL REPORTS THE DESCRIPTION IT HOLDS: "A description of the address pool"
//!    (`PublicIpv4Pool$Description`) is reported for every pool, carrying the text the pool
//!    was created with and the EMPTY text for a pool created without one.
//! 5. THE READ AND THE MUTATION ANSWER THEIR OWN NOT-FOUND CODES for the same absent pool:
//!    `DescribePublicIpv4Pools` refuses `InvalidPublicIpv4PoolID.NotFound` where
//!    `DeletePublicIpv4Pool` refuses `InvalidPublicIpv4Pool.NotFound`, both under the one
//!    message naming the id the request sent.
//! 6. A REPORTED SUBNET CARRIES THE DHCP OPTION SET IN FORCE FOR IT and the scope that set
//!    came from: a subnet holds no option set of its own, so the set is the one its VPC is
//!    associated with and the scope is the same `vpc` a VPC names — read off the VPC on
//!    every read, so moving the VPC's association moves every subnet of it. Neither element
//!    is in the model's member list for `com.amazonaws.ec2#Subnet`, so the recording is the
//!    authority for both, and rule 6b pins the derivation by taking it away. Neither element
//!    depends on HOW the call reached the subnet or WHICH operation answered with it: a
//!    selection by id, a filtered read, every page of a paged walk of that same selection and
//!    the create that mints a Region's default subnet back all report them.
//! 7. A RESOLVED PRIVATE DNS NAME BELONGS TO AN INTERFACE'S PRIMARY PRIVATE ADDRESS: inside
//!    `privateIpAddressesSet` only the primary entry carries a name, and every secondary
//!    entry carries the member EMPTY — whichever call put the address on the interface and
//!    whichever operation reports it.
//! 8. A FLOW LOG THAT PUBLISHES TO AMAZON S3 REPORTS ITS DESTINATION OPTIONS, each member at
//!    the value in force for it: the value the create asked for, or the default its own
//!    documentation gives it. A flow log of any other destination type reports none — the
//!    options describe the objects an Amazon S3 delivery writes, so they are that
//!    destination's shape.
//! 9. A PAGED `DescribeFlowLogs` ANSWERS A `NextToken` WHENEVER `MaxResults` FILLED THE
//!    PAGE, and the walk ends on the page the limit did not fill.
//! 10. A LAUNCH TEMPLATE NAMES THE IAM PRINCIPAL THAT CREATED IT — the identity the call
//!     speaks for, not the account root — and its latest version number follows the
//!     versions it holds.
//!
//! Each table row is a distinct input along what the rules generalise over — which
//! operation reports the shape, how many of the shape the response carries, whether the
//! record carries the value the element is derived from, what the create asked for, how many
//! addresses an interface holds and how they got there, how many items a page holds against
//! the limit asked for — so no single recorded call is the specification.

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, PublicIpv4PoolSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    call, element, elements, emulator, error_code, error_message, filters, indexed,
};
use super::wire::{child, child_text, document, member, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The scope `dhcpOptionsSource` names for a DHCP option set associated with the VPC
/// itself, which is where every set this store associates is associated.
const DHCP_OPTIONS_SOURCE_VPC: &str = "vpc";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One call that must succeed, answering its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &Params) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// RULE 1, on the VPC side: every VPC a response reports carries the scope its DHCP option
/// set comes from, whichever operation reports it and however many VPCs the response
/// carries.
#[tokio::test]
async fn every_reported_vpc_carries_the_scope_of_its_dhcp_option_set() {
    let (router, _engine) = emulator();

    // Rows: the operation reporting the VPC, and how many VPCs its response carries. They
    // span the three positions the shape is reported in — a member of a create's result, a
    // one-item list, and a list of several — and both kinds of VPC the store holds: the
    // account's ambient default VPC and the ones these creates mint.
    let created = ok(
        &router,
        "CreateVpc",
        "CreateVpc",
        &vec![p("CidrBlock", "10.0.0.0/16")],
    )
    .await;
    let created_id = element(&created, "vpcId").expect("the create reports the VPC it minted");
    let dual = ok(
        &router,
        "CreateVpc with an IPv6 block",
        "CreateVpc",
        &vec![
            p("CidrBlock", "10.1.0.0/16"),
            p("AmazonProvidedIpv6CidrBlock", "true"),
        ],
    )
    .await;

    let one = ok(
        &router,
        "DescribeVpcs naming one VPC",
        "DescribeVpcs",
        &indexed("VpcId", &[&created_id]),
    )
    .await;
    // Every VPC of the Region: the two creates above beside the account's default VPC.
    let all = ok(
        &router,
        "DescribeVpcs naming none",
        "DescribeVpcs",
        &Vec::new(),
    )
    .await;

    let rows: [(&str, &str, usize); 4] = [
        ("CreateVpc", &created, 1),
        ("CreateVpc with an IPv6 block", &dual, 1),
        ("DescribeVpcs naming one VPC", &one, 1),
        ("DescribeVpcs naming none", &all, 3),
    ];
    for (case, body, expected_vpcs) in rows {
        let ids = elements(body, "vpcId");
        assert_eq!(
            ids.len(),
            expected_vpcs,
            "case {case:?}: expected {expected_vpcs} reported VPC(s), got {ids:?} in {body}"
        );
        let sources = elements(body, "dhcpOptionsSource");
        assert_eq!(
            sources,
            vec![DHCP_OPTIONS_SOURCE_VPC.to_string(); expected_vpcs],
            "case {case:?}: expected one dhcpOptionsSource {DHCP_OPTIONS_SOURCE_VPC:?} per \
             reported VPC, got {sources:?} in {body}"
        );
        // The scope names the association the VPC reports, so there is exactly one of each.
        let ids = elements(body, "dhcpOptionsId");
        assert_eq!(
            ids.len(),
            sources.len(),
            "case {case:?}: expected a scope for each reported option set, got {ids:?} \
             against {sources:?} in {body}"
        );
    }
}

/// RULE 2: the scope is read off the record, not written onto every VPC — a VPC the store
/// holds no option set association for reports no scope at all.
#[tokio::test]
async fn a_vpc_with_no_option_set_association_reports_no_scope() {
    let (router, engine) = emulator();
    let created = ok(
        &router,
        "a VPC whose association is dropped",
        "CreateVpc",
        &vec![p("CidrBlock", "10.0.0.0/16")],
    )
    .await;
    let vpc_id = element(&created, "vpcId").expect("the create reports the VPC it minted");

    // Take the association away in the store, which is the only thing the element is
    // derived from.
    {
        let state = engine.state::<Ec2State>();
        let mut store = state.lock();
        let vpc = store
            .vpcs
            .get_mut(&vpc_id)
            .expect("the store holds the VPC the create minted");
        vpc.dhcp_options_id = None;
    }

    let described = ok(
        &router,
        "a VPC whose association is dropped",
        "DescribeVpcs",
        &indexed("VpcId", &[&vpc_id]),
    )
    .await;
    assert_eq!(
        element(&described, "dhcpOptionsId"),
        None,
        "case \"a VPC whose association is dropped\": the association is gone, got {described}"
    );
    assert_eq!(
        element(&described, "dhcpOptionsSource"),
        None,
        "case \"a VPC whose association is dropped\": expected no scope for a VPC that has no \
         option set, got {described}"
    );
}

/// RULE 1, on the Verified Access side: every instance a response reports carries its
/// transit-gateway attachment set, present and empty for an instance holding none —
/// whichever operation reports the instance.
#[tokio::test]
async fn every_reported_verified_access_instance_carries_its_attachment_set() {
    const ATTACHMENT_SET: &str = "verifiedAccessInstanceTransitGatewayAttachmentSet";

    let (router, _engine) = emulator();
    let created = ok(
        &router,
        "CreateVerifiedAccessInstance",
        "CreateVerifiedAccessInstance",
        &vec![p("Description", "the sales applications")],
    )
    .await;
    let instance_id =
        element(&created, "verifiedAccessInstanceId").expect("the create reports the instance");
    let second = ok(
        &router,
        "a second instance",
        "CreateVerifiedAccessInstance",
        &Vec::new(),
    )
    .await;
    assert_eq!(
        elements(&second, ATTACHMENT_SET).len(),
        1,
        "case \"a second instance\": expected the attachment set on it too, got {second}"
    );

    let modified = ok(
        &router,
        "ModifyVerifiedAccessInstance",
        "ModifyVerifiedAccessInstance",
        &vec![
            p("VerifiedAccessInstanceId", instance_id.clone()),
            p("Description", "the support applications"),
        ],
    )
    .await;
    let one = ok(
        &router,
        "DescribeVerifiedAccessInstances naming one",
        "DescribeVerifiedAccessInstances",
        &indexed("VerifiedAccessInstanceId", &[&instance_id]),
    )
    .await;
    let both = ok(
        &router,
        "DescribeVerifiedAccessInstances naming none",
        "DescribeVerifiedAccessInstances",
        &Vec::new(),
    )
    .await;

    // Rows: the operation reporting the instance, and how many instances its response
    // carries — a member of a create's result, of a modify's result, a one-item list and a
    // list of two.
    let rows: [(&str, &str, usize); 4] = [
        ("CreateVerifiedAccessInstance", &created, 1),
        ("ModifyVerifiedAccessInstance", &modified, 1),
        ("DescribeVerifiedAccessInstances naming one", &one, 1),
        ("DescribeVerifiedAccessInstances naming none", &both, 2),
    ];
    for (case, body, expected_instances) in rows {
        let ids = elements(body, "verifiedAccessInstanceId");
        assert_eq!(
            ids.len(),
            expected_instances,
            "case {case:?}: expected {expected_instances} reported instance(s), got {ids:?} in \
             {body}"
        );
        let sets = elements(body, ATTACHMENT_SET);
        assert_eq!(
            sets,
            vec![String::new(); expected_instances],
            "case {case:?}: expected one empty {ATTACHMENT_SET} per reported instance, got \
             {sets:?} in {body}"
        );
    }
}

/// RULE 3: the create answers the transient state the VPC was minted in, and every call
/// after it reports `available`.
#[tokio::test]
async fn a_vpc_is_available_once_the_create_that_made_it_has_returned() {
    // Rows: the two creates that mint a VPC, and the shapes a `CreateVpc` can take — an
    // IPv4 block alone, one with an IPv6 block beside it, and one carrying a tag.
    type Row = (&'static str, &'static str, Params);
    let rows: Vec<Row> = vec![
        (
            "an IPv4 block alone",
            "CreateVpc",
            vec![p("CidrBlock", "10.0.0.0/16")],
        ),
        (
            "an IPv6 block beside it",
            "CreateVpc",
            vec![
                p("CidrBlock", "10.1.0.0/16"),
                p("AmazonProvidedIpv6CidrBlock", "true"),
            ],
        ),
        (
            "a tag on the VPC",
            "CreateVpc",
            vec![
                p("CidrBlock", "10.2.0.0/16"),
                p("TagSpecification.1.ResourceType", "vpc"),
                p("TagSpecification.1.Tag.1.Key", "Name"),
                p("TagSpecification.1.Tag.1.Value", "web"),
            ],
        ),
    ];

    for (case, action, params) in rows {
        // One emulator per row: each row is an independent input, not a sequence.
        let (router, _engine) = emulator();
        let created = ok(&router, case, action, &params).await;
        // The VPC's OWN state, read through the tree: an association inside it carries a
        // `<state>` of its own.
        assert_eq!(
            member(case, &created, "state"),
            "pending",
            "case {case:?}: the create answers the state the VPC was minted in, got {created}"
        );
        let vpc_id = element(&created, "vpcId").expect("the create reports the VPC it minted");

        // Every call after the create: the state is a property of the clock, so the second
        // read reports exactly what the first did.
        for nth in 0..2 {
            let described = ok(&router, case, "DescribeVpcs", &indexed("VpcId", &[&vpc_id])).await;
            assert_eq!(
                member(case, &described, "state"),
                "available",
                "case {case:?}: read {nth}: a VPC is available once its create has returned, \
                 got {described}"
            );
        }
    }
}

/// RULE 4: a pool reports the description it holds — the text it was created with, and the
/// empty text for a pool created without one.
#[tokio::test]
async fn every_reported_pool_carries_the_description_it_holds() {
    // Rows differing along what the description can be: absent from the create (which is
    // every `CreatePublicIpv4Pool`, the request carrying no description member), empty text,
    // one word, and text with a space and a character outside ASCII.
    let seeded: Vec<(&str, Option<&str>, &str)> = vec![
        ("no description at all", None, ""),
        ("an empty description", Some(""), ""),
        ("one word", Some("byoip"), "byoip"),
        (
            "text with a space and one outside ASCII",
            Some("the Équipe range"),
            "the Équipe range",
        ),
    ];

    for (case, description, expected) in seeded {
        let (router, engine) = emulator();
        let pool_id = {
            let state = engine.state::<Ec2State>();
            let mut store = state.lock();
            store
                .insert_public_ipv4_pool(PublicIpv4PoolSpec {
                    description: description.map(str::to_string),
                    network_border_group: REGION.to_string(),
                    tags: Vec::new(),
                    cidrs: vec!["81.3.69.0/24".to_string()],
                })
                .expect("the store has room for the seeded pool")
        };
        let described = ok(
            &router,
            case,
            "DescribePublicIpv4Pools",
            &indexed("PoolId", &[&pool_id]),
        )
        .await;
        assert_eq!(
            element(&described, "description").as_deref(),
            Some(expected),
            "case {case:?}: expected the description {expected:?} on the reported pool, got \
             {described}"
        );
    }

    // And the pool the create under test mints: the request carries no description member,
    // so the pool holds the empty one and the describe still reports the element.
    let (router, _engine) = emulator();
    let created = ok(
        &router,
        "CreatePublicIpv4Pool",
        "CreatePublicIpv4Pool",
        &Vec::new(),
    )
    .await;
    let pool_id = element(&created, "poolId").expect("the create reports the pool it minted");
    let described = ok(
        &router,
        "CreatePublicIpv4Pool",
        "DescribePublicIpv4Pools",
        &indexed("PoolId", &[&pool_id]),
    )
    .await;
    assert_eq!(
        element(&described, "description").as_deref(),
        Some(""),
        "case \"CreatePublicIpv4Pool\": expected an empty description on a pool created \
         without one, got {described}"
    );
}

/// RULE 5: the READ of the pools and a request that ACTS on one answer DIFFERENT not-found
/// codes for the same absent pool, under the one message both carry.
#[tokio::test]
async fn the_read_and_the_delete_answer_their_own_not_found_codes() {
    /// The code `DescribePublicIpv4Pools` refuses an absent pool with.
    const READ_CODE: &str = "InvalidPublicIpv4PoolID.NotFound";
    /// The code `DeletePublicIpv4Pool` refuses the same absent pool with.
    const MUTATION_CODE: &str = "InvalidPublicIpv4Pool.NotFound";

    // Rows: one id of each shape a caller could send — the 17-hex-digit form EC2 mints, a
    // short legacy-length one, an id of another pool family, and a value that is no pool id
    // at all — so the codes are about the pool not being the account's rather than about the
    // id's shape. The last row is a pool the account DID hold and deleted, so "absent"
    // covers both the never-held and the just-removed id.
    let unheld: [(&str, &str); 4] = [
        (
            "a well-formed id nothing holds",
            "ipv4pool-ec2-00000000000000000",
        ),
        ("a short id", "ipv4pool-ec2-1a2b3c4d"),
        ("an IPv6 pool id", "ipv6pool-ec2-0abcdef1234567890"),
        ("a value that is no pool id", "not-a-pool"),
    ];

    let (router, _engine) = emulator();
    // A pool the account DOES hold, deleted below, so the read's code is also what a pool
    // that existed and went answers.
    let held = element(
        &ok(
            &router,
            "a deleted pool",
            "CreatePublicIpv4Pool",
            &Vec::new(),
        )
        .await,
        "poolId",
    )
    .expect("the create reports the pool it minted");
    ok(
        &router,
        "a deleted pool",
        "DeletePublicIpv4Pool",
        &vec![p("PoolId", held.clone())],
    )
    .await;

    let mut rows: Vec<(&str, String)> = unheld
        .iter()
        .map(|(case, pool_id)| (*case, (*pool_id).to_string()))
        .collect();
    rows.push(("a deleted pool", held));

    for (case, pool_id) in rows {
        let expected_message = format!("The pool ID '{pool_id}' does not exist.");
        for (action, expected_code) in [
            ("DescribePublicIpv4Pools", READ_CODE),
            ("DeletePublicIpv4Pool", MUTATION_CODE),
        ] {
            let params = match action {
                "DescribePublicIpv4Pools" => indexed("PoolId", &[&pool_id]),
                _ => vec![p("PoolId", pool_id.clone())],
            };
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, expected_code,
                "case {case:?}: expected {action} to refuse {expected_code}, got {code:?} in \
                 {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {action} to answer {expected_message:?}, got \
                 {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 6: the DHCP option set in force for a reported subnet
// ---------------------------------------------------------------------------

/// One reported subnet, as rule 6 reads it: its id, the option set in force for it and the
/// scope that set came from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedSubnet {
    /// `Subnet$SubnetId` — which subnet the row is about.
    subnet_id: String,
    /// The `dhcpOptionsId` element the live wire renders on it, if any.
    dhcp_options_id: Option<String>,
    /// The `dhcpOptionsSource` element beside it, if any.
    dhcp_options_source: Option<String>,
}

/// Every subnet a response reports, read through the XML DOCUMENT so an element of the
/// SUBNET is never confused with one of the same name elsewhere in the body.
fn reported_subnets(case: &str, body: &str) -> Vec<ReportedSubnet> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "subnetId").is_some())
        .map(|node| ReportedSubnet {
            subnet_id: child_text(node, "subnetId").unwrap_or_default(),
            dhcp_options_id: child_text(node, "dhcpOptionsId"),
            dhcp_options_source: child_text(node, "dhcpOptionsSource"),
        })
        .collect()
}

/// A subnet of `vpc_id` created over the wire, as the id the create minted — asserting on
/// the way out that the create's OWN answer already carries the two elements.
async fn create_subnet(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    cidr_block: &str,
    expected_options: &str,
) -> String {
    let created = ok(
        router,
        case,
        "CreateSubnet",
        &vec![p("VpcId", vpc_id), p("CidrBlock", cidr_block)],
    )
    .await;
    let reported = reported_subnets(case, &created);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one reported subnet, got {reported:?} in {created}"
    );
    assert_eq!(
        reported[0].dhcp_options_id.as_deref(),
        Some(expected_options),
        "case {case:?}: expected the create to report the VPC's option set, got {reported:?}"
    );
    assert_eq!(
        reported[0].dhcp_options_source.as_deref(),
        Some(DHCP_OPTIONS_SOURCE_VPC),
        "case {case:?}: expected the scope {DHCP_OPTIONS_SOURCE_VPC:?}, got {reported:?}"
    );
    reported[0].subnet_id.clone()
}

/// RULE 6: every subnet a response reports carries the option set in force for it and the
/// scope it came from — whichever operation reports the subnet, however many subnets the
/// response carries, and whichever set the subnet's VPC is pointed at.
#[tokio::test]
async fn every_reported_subnet_carries_the_dhcp_option_set_in_force_for_it() {
    let (router, _engine) = emulator();

    let vpc = ok(
        &router,
        "the VPC the subnets inherit from",
        "CreateVpc",
        &vec![p("CidrBlock", "10.30.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");
    let vpc_options =
        element(&vpc, "dhcpOptionsId").expect("a created VPC is associated with an option set");

    let first = create_subnet(
        &router,
        "CreateSubnet",
        &vpc_id,
        "10.30.1.0/24",
        &vpc_options,
    )
    .await;
    let second = create_subnet(
        &router,
        "a second subnet of the same VPC",
        &vpc_id,
        "10.30.2.0/24",
        &vpc_options,
    )
    .await;

    let one = ok(
        &router,
        "DescribeSubnets naming one subnet",
        "DescribeSubnets",
        &indexed("SubnetId", &[&first]),
    )
    .await;
    let both = ok(
        &router,
        "DescribeSubnets naming both",
        "DescribeSubnets",
        &indexed("SubnetId", &[&first, &second]),
    )
    .await;

    // Rows: the operation reporting the subnet, and how many subnets its response carries —
    // a one-item list and a list of two, beside the two create answers asserted above.
    let rows: [(&str, &str, usize); 2] = [
        ("DescribeSubnets naming one subnet", &one, 1),
        ("DescribeSubnets naming both", &both, 2),
    ];
    for (case, body, expected_subnets) in rows {
        let reported = reported_subnets(case, body);
        assert_eq!(
            reported.len(),
            expected_subnets,
            "case {case:?}: expected {expected_subnets} reported subnet(s), got {reported:?} \
             in {body}"
        );
        for subnet in &reported {
            assert_eq!(
                subnet.dhcp_options_id.as_deref(),
                Some(vpc_options.as_str()),
                "case {case:?}: expected the VPC's option set on every reported subnet, got \
                 {subnet:?}"
            );
            assert_eq!(
                subnet.dhcp_options_source.as_deref(),
                Some(DHCP_OPTIONS_SOURCE_VPC),
                "case {case:?}: expected the scope {DHCP_OPTIONS_SOURCE_VPC:?} on every \
                 reported subnet, got {subnet:?}"
            );
        }
    }

    // Every subnet of the Region, the account's ambient default subnets included: each one
    // reports an option set of the account and the scope it came from, so the elements are a
    // property of a reported subnet rather than of the subnets this suite created.
    let all = ok(
        &router,
        "DescribeSubnets naming none",
        "DescribeSubnets",
        &Vec::new(),
    )
    .await;
    let reported = reported_subnets("DescribeSubnets naming none", &all);
    assert!(
        reported.len() > 2,
        "case \"DescribeSubnets naming none\": expected the account's default subnets beside \
         the two created here, got {reported:?}"
    );
    for subnet in &reported {
        assert!(
            subnet
                .dhcp_options_id
                .as_deref()
                .is_some_and(|id| id.starts_with("dopt-")),
            "case \"DescribeSubnets naming none\": expected an option set id on every reported \
             subnet, got {subnet:?}"
        );
        assert_eq!(
            subnet.dhcp_options_source.as_deref(),
            Some(DHCP_OPTIONS_SOURCE_VPC),
            "case \"DescribeSubnets naming none\": expected the scope \
             {DHCP_OPTIONS_SOURCE_VPC:?} on every reported subnet, got {subnet:?}"
        );
    }

    // The set is READ off the VPC on every read: pointing the VPC at another option set
    // moves every subnet of it, with nothing written on the subnets themselves.
    let options = ok(
        &router,
        "another option set",
        "CreateDhcpOptions",
        &vec![
            p("DhcpConfiguration.1.Key", "domain-name-servers"),
            p("DhcpConfiguration.1.Value.1", "10.30.0.2"),
        ],
    )
    .await;
    let moved_to =
        element(&options, "dhcpOptionsId").expect("the create reports the option set it minted");
    ok(
        &router,
        "the VPC pointed at another option set",
        "AssociateDhcpOptions",
        &vec![p("VpcId", vpc_id.clone()), p("DhcpOptionsId", moved_to.clone())],
    )
    .await;
    let after = ok(
        &router,
        "the VPC pointed at another option set",
        "DescribeSubnets",
        &indexed("SubnetId", &[&first, &second]),
    )
    .await;
    for subnet in reported_subnets("the VPC pointed at another option set", &after) {
        assert_eq!(
            subnet.dhcp_options_id.as_deref(),
            Some(moved_to.as_str()),
            "case \"the VPC pointed at another option set\": expected the set the VPC now \
             holds, got {subnet:?}"
        );
        assert_eq!(
            subnet.dhcp_options_source.as_deref(),
            Some(DHCP_OPTIONS_SOURCE_VPC),
            "case \"the VPC pointed at another option set\": expected the scope to stay, got \
             {subnet:?}"
        );
    }
}

/// RULE 6b: the set is read off the VPC, not written onto every subnet — a subnet whose VPC
/// holds no option set association reports neither element.
#[tokio::test]
async fn a_subnet_whose_vpc_has_no_option_set_reports_neither_element() {
    let (router, engine) = emulator();
    let case = "a VPC whose association is dropped";
    let vpc = ok(&router, case, "CreateVpc", &vec![p("CidrBlock", "10.31.0.0/16")]).await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");
    let vpc_options =
        element(&vpc, "dhcpOptionsId").expect("a created VPC is associated with an option set");
    let subnet_id = create_subnet(&router, case, &vpc_id, "10.31.1.0/24", &vpc_options).await;

    // Take the VPC's association away in the store, which is the only thing the two elements
    // are derived from.
    {
        let state = engine.state::<Ec2State>();
        let mut store = state.lock();
        let vpc = store
            .vpcs
            .get_mut(&vpc_id)
            .expect("the store holds the VPC the create minted");
        vpc.dhcp_options_id = None;
    }

    let described = ok(
        &router,
        case,
        "DescribeSubnets",
        &indexed("SubnetId", &[&subnet_id]),
    )
    .await;
    let reported = reported_subnets(case, &described);
    assert_eq!(
        reported,
        vec![ReportedSubnet {
            subnet_id: subnet_id.clone(),
            dhcp_options_id: None,
            dhcp_options_source: None,
        }],
        "case {case:?}: expected no option set and no scope for a subnet whose VPC has none, \
         got {reported:?} in {described}"
    );
}

/// The page size the walk below asks for: the SMALLEST page `DescribeSubnets` publishes
/// (`DescribeSubnetsRequest$MaxResults`, `range 5..1000`), so one more subnet than it spills
/// the read onto a second page.
const SUBNET_PAGE_SIZE: usize = 5;

/// More pages than the walk below can need: a walk that reaches this is not paging at all.
const MOST_SUBNET_PAGES: usize = 8;

/// One page of a `DescribeSubnets` walk: the subnets the page reported, and the `NextToken`
/// it answered with.
type SubnetPage = (Vec<ReportedSubnet>, Option<String>);

/// The pages a `DescribeSubnets` walk over the subnets of `vpc_id` reports: the subnets each
/// page carried and the `NextToken` that page answered with.
///
/// The walk follows the token the service minted, which is the only thing a client can do
/// with one, and is bounded so a token that never advanced fails the case instead of looping
/// for ever.
async fn subnet_pages(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    max_results: Option<usize>,
) -> Vec<SubnetPage> {
    let mut pages: Vec<SubnetPage> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut params: Params = filters(&[("vpc-id", &[vpc_id])]);
        if let Some(limit) = max_results {
            params.push(p("MaxResults", limit.to_string()));
        }
        if let Some(held) = token.clone() {
            params.push(p("NextToken", held));
        }
        let body = ok(router, case, "DescribeSubnets", &params).await;
        let next = super::ec2_query::member(&body, "nextToken");
        pages.push((reported_subnets(case, &body), next.clone()));
        assert!(
            pages.len() <= MOST_SUBNET_PAGES,
            "case {case:?}: the walk did not end within {MOST_SUBNET_PAGES} pages, got {pages:?}"
        );
        match next {
            Some(held) => token = Some(held),
            None => break,
        }
    }
    pages
}

/// RULE 6, over the two things a reported subnet does NOT depend on: HOW the request reached
/// the subnet, and WHICH operation answered with it.
///
/// The rows are a filtered read, each page of a paged walk of the same selection, and the
/// create that mints a Region's default subnet back — every one of them reports the option
/// set in force for the subnet and the scope it came from, so neither element is a property
/// of one selection form or of one operation.
#[tokio::test]
async fn every_reported_subnet_carries_the_set_however_the_call_reached_it() {
    let (router, _engine) = emulator();

    // One VPC with one subnet more than a page holds, so the paged rows below have a page
    // the limit filled and a page it did not.
    let case = "a VPC with more subnets than one page holds";
    let vpc = ok(&router, case, "CreateVpc", &vec![p("CidrBlock", "10.40.0.0/16")]).await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");
    let vpc_options =
        element(&vpc, "dhcpOptionsId").expect("a created VPC is associated with an option set");
    let mut created: Vec<String> = Vec::new();
    for nth in 1..=SUBNET_PAGE_SIZE + 1 {
        created.push(
            create_subnet(
                &router,
                case,
                &vpc_id,
                &format!("10.40.{nth}.0/24"),
                &vpc_options,
            )
            .await,
        );
    }
    created.sort();

    let paged = subnet_pages(
        &router,
        "a paged walk of the VPC's subnets",
        &vpc_id,
        Some(SUBNET_PAGE_SIZE),
    )
    .await;
    let unpaged = subnet_pages(
        &router,
        "the VPC's subnets in one page",
        &vpc_id,
        None,
    )
    .await;

    // The shape of each walk: the limit filled the first page and left a token, the page it
    // did not fill ended the walk, and a read that named no limit took one page.
    let shapes: [(&str, &[SubnetPage], &[usize]); 2] = [
        (
            "a paged walk of the VPC's subnets",
            &paged,
            &[SUBNET_PAGE_SIZE, 1],
        ),
        (
            "the VPC's subnets in one page",
            &unpaged,
            &[SUBNET_PAGE_SIZE + 1],
        ),
    ];
    for (case, walk, expected_pages) in shapes {
        let counts: Vec<usize> = walk.iter().map(|(subnets, _)| subnets.len()).collect();
        assert_eq!(
            counts, expected_pages,
            "case {case:?}: expected pages of {expected_pages:?} subnet(s), got {counts:?}"
        );
        let last_token = walk
            .last()
            .map(|(_, token)| token.clone())
            .expect("every walk reports at least one page");
        assert_eq!(
            last_token, None,
            "case {case:?}: the page the limit did not fill ends the walk, got {last_token:?}"
        );

        // RULE 6 itself: every subnet of every page, however the walk reached it.
        let mut walked: Vec<String> = Vec::new();
        for (subnets, _) in walk {
            for subnet in subnets {
                assert_eq!(
                    subnet.dhcp_options_id.as_deref(),
                    Some(vpc_options.as_str()),
                    "case {case:?}: expected the VPC's option set on every reported subnet, \
                     got {subnet:?}"
                );
                assert_eq!(
                    subnet.dhcp_options_source.as_deref(),
                    Some(DHCP_OPTIONS_SOURCE_VPC),
                    "case {case:?}: expected the scope {DHCP_OPTIONS_SOURCE_VPC:?} on every \
                     reported subnet, got {subnet:?}"
                );
                walked.push(subnet.subnet_id.clone());
            }
        }
        walked.sort();
        assert_eq!(
            walked, created,
            "case {case:?}: expected the walk to report every subnet of the VPC, got {walked:?}"
        );
    }

    // The ambient default subnet of one Availability Zone, which nothing here created: taking
    // it away and letting `CreateDefaultSubnet` mint it back reports the two elements off the
    // DEFAULT VPC's association, so the operation that answers with a subnet is as much
    // subject to the rule as the read is.
    let case = "the default subnet of a zone, remade";
    let defaults = ok(
        &router,
        case,
        "DescribeSubnets",
        &filters(&[("default-for-az", &["true"])]),
    )
    .await;
    let reported = reported_subnets(case, &defaults);
    let held = reported
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the account holds default subnets: {defaults}"));
    let default_options = held
        .dhcp_options_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: a default subnet reports an option set: {held:?}"));
    let described = ok(
        &router,
        case,
        "DescribeSubnets",
        &indexed("SubnetId", &[&held.subnet_id]),
    )
    .await;
    let zone = element(&described, "availabilityZone")
        .unwrap_or_else(|| panic!("case {case:?}: a subnet reports its zone: {described}"));
    let _ = ok(
        &router,
        case,
        "DeleteSubnet",
        &vec![p("SubnetId", held.subnet_id.clone())],
    )
    .await;
    let remade = ok(
        &router,
        case,
        "CreateDefaultSubnet",
        &vec![p("AvailabilityZone", zone)],
    )
    .await;
    let minted = reported_subnets(case, &remade);
    assert_eq!(
        minted.len(),
        1,
        "case {case:?}: expected exactly one reported subnet, got {minted:?} in {remade}"
    );
    assert_ne!(
        minted[0].subnet_id, held.subnet_id,
        "case {case:?}: the remade default subnet is a new resource, got {minted:?}"
    );
    // And the same answer on the way back out through the read, the subnet now being one the
    // account holds rather than one a create just answered with.
    let read_back = ok(
        &router,
        case,
        "DescribeSubnets",
        &indexed("SubnetId", &[&minted[0].subnet_id]),
    )
    .await;
    let read_back_subnets = reported_subnets(case, &read_back);
    let answers: [(&str, &[ReportedSubnet]); 2] = [
        ("the create", &minted),
        ("the read back", &read_back_subnets),
    ];
    for (row, subnets) in answers {
        assert!(
            !subnets.is_empty(),
            "case {case:?}: expected {row} to report the default subnet it minted"
        );
        for subnet in subnets {
            assert_eq!(
                subnet.dhcp_options_id.as_deref(),
                Some(default_options.as_str()),
                "case {case:?}: expected {row} to report the default VPC's option set, got \
                 {subnet:?}"
            );
            assert_eq!(
                subnet.dhcp_options_source.as_deref(),
                Some(DHCP_OPTIONS_SOURCE_VPC),
                "case {case:?}: expected {row} to report the scope {DHCP_OPTIONS_SOURCE_VPC:?}, \
                 got {subnet:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 7: a private DNS name belongs to the PRIMARY private address
// ---------------------------------------------------------------------------

/// One entry of an interface's `privateIpAddressesSet`, as rule 7 reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAddress {
    /// `NetworkInterfacePrivateIpAddress$PrivateIpAddress`.
    private_ip_address: Option<String>,
    /// `$Primary`, as the wire renders the boolean.
    primary: Option<String>,
    /// `$PrivateDnsName` — absent, empty or a resolved name.
    private_dns_name: Option<String>,
}

/// The private IPv4 DNS hostname AWS resolves for the PRIMARY private address of an
/// interface in [`REGION`]: `ip-<dash-separated address>` under the Region's private DNS
/// domain, which is `<region>.compute.internal` outside `us-east-1` (Amazon VPC User Guide,
/// "DHCP option sets"). Written out here rather than read from the crate, so a wrong name
/// cannot assert against itself.
fn resolved_private_dns_name(private_ip: &str) -> String {
    format!(
        "ip-{host}.{REGION}.compute.internal",
        host = private_ip.replace('.', "-")
    )
}

/// The addresses a response reports for the interface `network_interface_id`, read through
/// the XML document — so the same reading serves the interface's own report and the one
/// nested inside a reported instance.
fn reported_addresses(case: &str, body: &str, network_interface_id: &str) -> Vec<ReportedAddress> {
    let parsed = document(case, body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref() == Some(network_interface_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no interface {network_interface_id} in {body}")
        });
    set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .map(|item| ReportedAddress {
            private_ip_address: child_text(item, "privateIpAddress"),
            primary: child_text(item, "primary"),
            private_dns_name: child_text(item, "privateDnsName"),
        })
        .collect()
}

/// RULE 7 over one interface's reported addresses: `expected` entries, exactly one of them
/// primary and carrying the name the Region resolves for its address, and every other entry
/// carrying `privateDnsName` present and EMPTY.
fn assert_only_the_primary_address_is_named(
    case: &str,
    addresses: &[ReportedAddress],
    expected: usize,
) {
    assert_eq!(
        addresses.len(),
        expected,
        "case {case:?}: expected {expected} reported address(es), got {addresses:?}"
    );
    let primaries: Vec<&ReportedAddress> = addresses
        .iter()
        .filter(|address| address.primary.as_deref() == Some("true"))
        .collect();
    assert_eq!(
        primaries.len(),
        1,
        "case {case:?}: expected exactly one primary address, got {addresses:?}"
    );
    let primary = primaries[0];
    let address = primary.private_ip_address.clone().unwrap_or_default();
    assert_eq!(
        primary.private_dns_name.as_deref(),
        Some(resolved_private_dns_name(&address).as_str()),
        "case {case:?}: expected the resolved name of the primary address {address:?}, got \
         {primary:?}"
    );
    for secondary in addresses
        .iter()
        .filter(|address| address.primary.as_deref() != Some("true"))
    {
        assert_eq!(
            secondary.private_dns_name.as_deref(),
            Some(""),
            "case {case:?}: expected an empty privateDnsName on the secondary address, got \
             {secondary:?}"
        );
    }
}

/// RULE 7: only the primary private address of an interface carries a resolved private DNS
/// name, whichever call put the addresses on the interface.
///
/// The rows differ in HOW the interface got its addresses — the primary alone, a count of
/// secondary addresses, addresses named one by one, and addresses added to a standing
/// interface by name and by count — which is everything the rule generalises over.
#[tokio::test]
async fn only_the_primary_private_address_of_an_interface_carries_a_name() {
    let (router, _engine) = emulator();
    let vpc = ok(
        &router,
        "the interfaces' VPC",
        "CreateVpc",
        &vec![p("CidrBlock", "10.32.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");
    let vpc_options =
        element(&vpc, "dhcpOptionsId").expect("a created VPC is associated with an option set");
    let subnet_id = create_subnet(
        &router,
        "the interfaces' subnet",
        &vpc_id,
        "10.32.1.0/24",
        &vpc_options,
    )
    .await;

    // Rows: the create that puts the addresses on the interface, and how many addresses the
    // interface then holds.
    type Row = (&'static str, Params, usize);
    let rows: Vec<Row> = vec![
        ("the primary address alone", Vec::new(), 1),
        (
            "a count of two secondary addresses",
            vec![p("SecondaryPrivateIpAddressCount", "2")],
            3,
        ),
        (
            "three addresses named one by one",
            vec![
                p("PrivateIpAddresses.1.PrivateIpAddress", "10.32.1.20"),
                p("PrivateIpAddresses.1.Primary", "true"),
                p("PrivateIpAddresses.2.PrivateIpAddress", "10.32.1.21"),
                p("PrivateIpAddresses.2.Primary", "false"),
                p("PrivateIpAddresses.3.PrivateIpAddress", "10.32.1.22"),
                p("PrivateIpAddresses.3.Primary", "false"),
            ],
            3,
        ),
    ];
    for (row, (case, extra, expected)) in rows.into_iter().enumerate() {
        // An address this row names for itself, so no two rows ask for one address.
        let named_address = format!("10.32.1.{}", 80 + row);
        let mut params = vec![p("SubnetId", subnet_id.clone())];
        params.extend(extra);
        let created = ok(&router, case, "CreateNetworkInterface", &params).await;
        let network_interface_id = element(&created, "networkInterfaceId")
            .expect("the create reports the interface it minted");
        // The create's own answer, and the read after it: one record, so both report the
        // same addresses.
        assert_only_the_primary_address_is_named(
            case,
            &reported_addresses(case, &created, &network_interface_id),
            expected,
        );
        let described = ok(
            &router,
            case,
            "DescribeNetworkInterfaces",
            &indexed("NetworkInterfaceId", &[&network_interface_id]),
        )
        .await;
        assert_only_the_primary_address_is_named(
            case,
            &reported_addresses(case, &described, &network_interface_id),
            expected,
        );

        // And the addresses an ASSIGNMENT adds to the interface that already exists: by
        // count, then by name.
        for (assignment, assigned, held) in [
            (
                "one more address by count",
                vec![p("SecondaryPrivateIpAddressCount", "1")],
                expected + 1,
            ),
            (
                "one more address by name",
                vec![p("PrivateIpAddress.1", named_address.clone())],
                expected + 2,
            ),
        ] {
            let mut params = vec![p("NetworkInterfaceId", network_interface_id.clone())];
            params.extend(assigned);
            let case = &format!("{case}, then {assignment}");
            ok(&router, case, "AssignPrivateIpAddresses", &params).await;
            let described = ok(
                &router,
                case,
                "DescribeNetworkInterfaces",
                &indexed("NetworkInterfaceId", &[&network_interface_id]),
            )
            .await;
            assert_only_the_primary_address_is_named(
                case,
                &reported_addresses(case, &described, &network_interface_id),
                held,
            );
        }
    }
}

/// RULE 7 through the OTHER operation that reports an interface's addresses: an instance's
/// own projection of the interface attached to it.
#[tokio::test]
async fn an_instance_reports_its_interface_addresses_under_the_same_rule() {
    let (router, _engine) = emulator();
    let vpc = ok(
        &router,
        "the instance's VPC",
        "CreateVpc",
        &vec![p("CidrBlock", "10.33.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");
    let vpc_options =
        element(&vpc, "dhcpOptionsId").expect("a created VPC is associated with an option set");
    let subnet_id = create_subnet(
        &router,
        "the instance's subnet",
        &vpc_id,
        "10.33.1.0/24",
        &vpc_options,
    )
    .await;

    let case = "an instance's primary interface";
    let launched = ok(
        &router,
        case,
        "RunInstances",
        &vec![
            // `RunInstances` only has to accept the id: which image the instance was
            // launched from has no bearing on the addresses of its interface.
            p("ImageId", "ami-0123456789abcdef0"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", subnet_id.clone()),
        ],
    )
    .await;
    let instance_id =
        element(&launched, "instanceId").expect("the launch reports the instance it minted");
    let network_interface_id =
        super::ec2_query::primary_network_interface(&router, REGION, case, &instance_id).await;

    // Rows: the instance as launched (its primary address alone), and after two more
    // addresses were assigned to the interface it holds.
    for (assignment, expected) in [("", 1), ("one more address", 2), ("two more", 3)] {
        if !assignment.is_empty() {
            ok(
                &router,
                case,
                "AssignPrivateIpAddresses",
                &vec![
                    p("NetworkInterfaceId", network_interface_id.clone()),
                    p("SecondaryPrivateIpAddressCount", "1"),
                ],
            )
            .await;
        }
        let described = ok(
            &router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        let addresses = reported_addresses(case, &described, &network_interface_id);
        assert_only_the_primary_address_is_named(
            &format!("{case} holding {expected} address(es)"),
            &addresses,
            expected,
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 8: a flow log's destination options, and RULE 9: its page token
// ---------------------------------------------------------------------------

/// The destination options a reported flow log carries: `(fileFormat,
/// hiveCompatiblePartitions, perHourPartition)`, each `None` when the element is absent.
type DestinationOptions = (Option<String>, Option<String>, Option<String>);

/// The destination options of the one flow log a response reports — `None` when the flow log
/// carries no `destinationOptions` element at all.
fn reported_destination_options(case: &str, body: &str) -> Option<DestinationOptions> {
    let parsed = document(case, body);
    let flow_log = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "flowLogId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no flow log (no <flowLogId>) in {body}"));
    let options = child(flow_log, "destinationOptions")?;
    Some((
        child_text(options, "fileFormat"),
        child_text(options, "hiveCompatiblePartitions"),
        child_text(options, "perHourPartition"),
    ))
}

/// RULE 8: a flow log that delivers to Amazon S3 reports its destination options whether or
/// not the create asked for them, each member at the value in force for it — the value the
/// create asked for, or the
/// default `DestinationOptionsRequest` documents for the member ("The default is plain-text"
/// for `$FileFormat`, "The default is false" for `$HiveCompatiblePartitions` and
/// `$PerHourPartition`). The options describe the OBJECTS an Amazon S3 delivery writes, which
/// is also why a request naming
/// them for another destination type is refused, so a flow log of any other type reports
/// none.
///
/// The rows differ in WHICH options the create named — none at all (twice: once for a
/// destination type that takes no options at all), one of the three, all three, and all
/// three spelled out at their own defaults.
#[tokio::test]
async fn a_flow_log_carries_the_destination_options_of_the_destination_it_publishes_to() {
    let (router, _engine) = emulator();
    let vpc = ok(
        &router,
        "the monitored VPC",
        "CreateVpc",
        &vec![p("CidrBlock", "10.34.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");

    /// The default every member a request leaves out takes.
    const PLAIN_TEXT: &str = "plain-text";
    const FALSE: &str = "false";

    // Rows: the destination the create publishes to, the destination options it names, and
    // the options the flow log must then report — `None` where the flow log reports none.
    type Row = (&'static str, Params, Option<DestinationOptions>);
    let rows: Vec<Row> = vec![
        (
            // A CloudWatch Logs destination takes no destination options at all, so this
            // row is the one where the member cannot even be asked for — and the flow log
            // reports none, the member being the Amazon S3 shape.
            "a CloudWatch Logs destination",
            super::flow_logs::cloud_watch_logs_params("VPC", &vpc_id, "destination-options"),
            None,
        ),
        (
            // An S3 destination that names no options still HAS an object layout: every
            // member is reported at its own documented default.
            "an S3 destination naming no options",
            s3_flow_log_params(&vpc_id, "none", &[]),
            Some((
                Some(PLAIN_TEXT.to_string()),
                Some(FALSE.to_string()),
                Some(FALSE.to_string()),
            )),
        ),
        (
            "an S3 destination naming the file format alone",
            s3_flow_log_params(&vpc_id, "parquet", &[("FileFormat", "parquet")]),
            Some((
                Some("parquet".to_string()),
                Some(FALSE.to_string()),
                Some(FALSE.to_string()),
            )),
        ),
        (
            "an S3 destination naming Hive-compatible prefixes alone",
            s3_flow_log_params(&vpc_id, "hive", &[("HiveCompatiblePartitions", "true")]),
            Some((
                Some(PLAIN_TEXT.to_string()),
                Some("true".to_string()),
                Some(FALSE.to_string()),
            )),
        ),
        (
            "an S3 destination naming all three",
            s3_flow_log_params(
                &vpc_id,
                "all",
                &[
                    ("FileFormat", "parquet"),
                    ("HiveCompatiblePartitions", "true"),
                    ("PerHourPartition", "true"),
                ],
            ),
            Some((
                Some("parquet".to_string()),
                Some("true".to_string()),
                Some("true".to_string()),
            )),
        ),
        (
            "an S3 destination spelling out the defaults",
            s3_flow_log_params(
                &vpc_id,
                "defaults",
                &[
                    ("FileFormat", PLAIN_TEXT),
                    ("HiveCompatiblePartitions", FALSE),
                    ("PerHourPartition", FALSE),
                ],
            ),
            Some((
                Some(PLAIN_TEXT.to_string()),
                Some(FALSE.to_string()),
                Some(FALSE.to_string()),
            )),
        ),
    ];

    for (case, params, expected) in rows {
        let created = ok(&router, case, "CreateFlowLogs", &params).await;
        let flow_log_id = element(&created, "item")
            .unwrap_or_else(|| panic!("case {case:?}: the create reports no flow log id: \
                                      {created}"));
        let described = ok(
            &router,
            case,
            "DescribeFlowLogs",
            &indexed("FlowLogId", &[&flow_log_id]),
        )
        .await;
        let reported = reported_destination_options(case, &described);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the destination options {expected:?} in force, got \
             {reported:?} in {described}"
        );
    }
}

/// A `CreateFlowLogs` on `vpc_id` publishing to an Amazon S3 bucket prefix of its own, with
/// `options` as its `DestinationOptions` members.
fn s3_flow_log_params(vpc_id: &str, prefix: &str, options: &[(&str, &str)]) -> Params {
    let mut params = vec![
        p("ResourceType", "VPC"),
        p("ResourceId.1", vpc_id),
        p("TrafficType", "ALL"),
        p("LogDestinationType", "s3"),
        p(
            "LogDestination",
            format!("arn:aws:s3:::flow-logs-bucket/{prefix}/"),
        ),
    ];
    for (member, value) in options {
        params.push(p(&format!("DestinationOptions.{member}"), *value));
    }
    params
}

/// The pages a `DescribeFlowLogs` walk reports: the flow log ids each page carried and the
/// `NextToken` it answered with.
///
/// The walk follows the token the service minted, which is the only thing a client can do
/// with one, and is bounded so a token that never advanced fails the case instead of looping
/// for ever.
async fn flow_log_pages(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<usize>,
) -> Vec<(Vec<String>, Option<String>)> {
    /// More pages than any row below needs: a walk that reaches this is not paging.
    const MOST_PAGES: usize = 8;

    let mut pages: Vec<(Vec<String>, Option<String>)> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut params: Params = Vec::new();
        if let Some(limit) = max_results {
            params.push(p("MaxResults", limit.to_string()));
        }
        if let Some(held) = token.clone() {
            params.push(p("NextToken", held));
        }
        let body = ok(router, case, "DescribeFlowLogs", &params).await;
        let next = super::ec2_query::member(&body, "nextToken");
        pages.push((elements(&body, "flowLogId"), next.clone()));
        assert!(
            pages.len() <= MOST_PAGES,
            "case {case:?}: the walk did not end within {MOST_PAGES} pages, got {pages:?}"
        );
        match next {
            Some(held) => token = Some(held),
            None => break,
        }
    }
    pages
}

/// RULE 9: a paged read answers a `NextToken` whenever `MaxResults` filled the page — so a
/// caller that asked for a page size and got it is told to ask again — and the walk ends on
/// the page the limit did not fill.
///
/// The rows differ in the page size asked for against the number of flow logs held: a limit
/// below the count, one that divides it, one equal to it, one above it, and none at all.
#[tokio::test]
async fn a_paged_flow_log_read_answers_a_token_whenever_the_page_was_filled() {
    let (router, _engine) = emulator();
    let vpc = ok(
        &router,
        "the monitored VPC",
        "CreateVpc",
        &vec![p("CidrBlock", "10.35.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").expect("the create reports the VPC it minted");

    // Three flow logs of the account, each delivering to a log group of its own so the three
    // are three distinct flow logs.
    let mut held: Vec<String> = Vec::new();
    for nth in 1..=3 {
        let case = "three flow logs";
        let created = ok(
            &router,
            case,
            "CreateFlowLogs",
            &super::flow_logs::cloud_watch_logs_params("VPC", &vpc_id, &format!("page-{nth}")),
        )
        .await;
        held.push(
            element(&created, "item")
                .unwrap_or_else(|| panic!("case {case:?}: the create reports no id: {created}")),
        );
    }
    // The read pages in `FlowLogId` order, which is the order the walk must report them in.
    held.sort();

    // Rows: the page size asked for, and the size of each page the walk must then report.
    let rows: Vec<(&str, Option<usize>, Vec<usize>)> = vec![
        ("a page size below the count", Some(1), vec![1, 1, 1, 0]),
        ("a page size that divides the count", Some(2), vec![2, 1]),
        ("a page size equal to the count", Some(3), vec![3, 0]),
        ("a page size above the count", Some(5), vec![3]),
        ("no page size at all", None, vec![3]),
    ];
    for (case, max_results, expected_sizes) in rows {
        let pages = flow_log_pages(&router, case, max_results).await;
        let sizes: Vec<usize> = pages.iter().map(|(ids, _)| ids.len()).collect();
        assert_eq!(
            sizes, expected_sizes,
            "case {case:?}: expected pages of {expected_sizes:?} flow logs, got {pages:?}"
        );
        // A token on every page the limit filled, and none on the page it did not — which is
        // also what ends the walk.
        for (nth, (ids, next)) in pages.iter().enumerate() {
            let filled = max_results.is_some_and(|limit| ids.len() == limit);
            assert_eq!(
                next.is_some(),
                filled,
                "case {case:?}: page {nth} carried {} of {max_results:?} flow logs, so a token \
                 was {}: got {next:?}",
                ids.len(),
                if filled { "due" } else { "not due" }
            );
        }
        // However the walk was paged, it reports each flow log of the account exactly once,
        // in id order.
        let walked: Vec<String> = pages.iter().flat_map(|(ids, _)| ids.clone()).collect();
        assert_eq!(
            walked, held,
            "case {case:?}: expected the walk to report every flow log once in id order, got \
             {walked:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 10: the principal a launch template names as its creator
// ---------------------------------------------------------------------------

/// RULE 10 over one reported `createdBy`: it is an IAM PRINCIPAL ARN of this account —
/// `arn:aws:iam::<account>:user/<name>`, the identity a long-term access key belongs to —
/// rather than the account root, which signs no request here.
fn assert_names_an_iam_principal(case: &str, created_by: &str) {
    let rest = created_by.strip_prefix("arn:aws:iam::").unwrap_or_else(|| {
        panic!("case {case:?}: expected an IAM ARN in createdBy, got {created_by:?}")
    });
    let (account, resource) = rest.split_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected an account segment in createdBy, got {created_by:?}")
    });
    assert!(
        account.len() == 12 && account.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected a 12-digit account id in createdBy, got {created_by:?}"
    );
    let name = resource.strip_prefix("user/").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the ARN of the IAM principal that created the template, \
             not {resource:?} (the account root creates nothing here): {created_by:?}"
        )
    });
    assert!(
        !name.is_empty(),
        "case {case:?}: expected a named IAM principal in createdBy, got {created_by:?}"
    );
}

/// RULE 10: every report of a launch template names the IAM principal that created it, and
/// the template's latest version number follows the versions it holds.
///
/// The rows differ in what the template data asked for and in how many versions the template
/// holds, and each is read back through both operations that report the template and the one
/// that reports its versions.
#[tokio::test]
async fn every_reported_launch_template_names_the_principal_that_created_it() {
    let (router, _engine) = emulator();

    // Rows: the one member of the launch template data the create names, so a row is a
    // template of its own rather than a repeat of one request.
    let rows: [(&str, &str, &str); 3] = [
        ("an image", "web", "ImageId=ami-0123456789abcdef0"),
        ("an instance type", "db.tier-1", "InstanceType=t3.micro"),
        ("a key pair", "Analytics/Reports", "KeyName=analytics-key"),
    ];
    for (case, name, member) in rows {
        let (field, value) = member
            .split_once('=')
            .expect("each row names one LaunchTemplateData member");
        let created = ok(
            &router,
            case,
            "CreateLaunchTemplate",
            &vec![
                p("LaunchTemplateName", name),
                p(&format!("LaunchTemplateData.{field}"), value),
            ],
        )
        .await;
        let created_by = element(&created, "createdBy")
            .unwrap_or_else(|| panic!("case {case:?}: the create reports no createdBy: {created}"));
        assert_names_an_iam_principal(case, &created_by);
        let launch_template_id = element(&created, "launchTemplateId")
            .expect("the create reports the template it minted");

        // A second version, so the latest version number has somewhere to move to.
        let version = ok(
            &router,
            case,
            "CreateLaunchTemplateVersion",
            &vec![
                p("LaunchTemplateId", launch_template_id.clone()),
                p("LaunchTemplateData.InstanceType", "t3.small"),
            ],
        )
        .await;
        assert_names_an_iam_principal(
            &format!("{case}, a second version"),
            &element(&version, "createdBy").unwrap_or_else(|| {
                panic!("case {case:?}: the version reports no createdBy: {version}")
            }),
        );

        let described = ok(
            &router,
            case,
            "DescribeLaunchTemplates",
            &indexed("LaunchTemplateId", &[&launch_template_id]),
        )
        .await;
        assert_eq!(
            element(&described, "createdBy").as_deref(),
            Some(created_by.as_str()),
            "case {case:?}: expected the read to report the creating principal the create \
             answered, got {described}"
        );
        assert_eq!(
            element(&described, "latestVersionNumber").as_deref(),
            Some("2"),
            "case {case:?}: expected the latest version to be the second one, got {described}"
        );

        // Every version of the template names a principal too, and the same one.
        let versions = ok(
            &router,
            case,
            "DescribeLaunchTemplateVersions",
            &vec![p("LaunchTemplateId", launch_template_id.clone())],
        )
        .await;
        let reported = elements(&versions, "createdBy");
        assert_eq!(
            reported,
            vec![created_by.clone(); 2],
            "case {case:?}: expected both versions to name the creating principal, got \
             {reported:?} in {versions}"
        );
    }
}
