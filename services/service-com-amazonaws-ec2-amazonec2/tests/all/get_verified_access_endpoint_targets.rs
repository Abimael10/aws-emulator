// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetVerifiedAccessEndpointTargets`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A NETWORK CIDR ENDPOINT'S TARGETS ARE THE ADDRESSES INSIDE ITS CIDR THAT A NETWORK
//!    INTERFACE OF THE VPC IT REACHES REALLY HOLDS: one target per such address, in address
//!    order, each naming the endpoint, the address, and the DNS name that address is reached
//!    under — the custom subdomain of the Verified Access INSTANCE when it has one, the
//!    endpoint's own generated domain otherwise. An address the CIDR merely covers but
//!    nothing occupies is no target, and neither is an address of another VPC — private
//!    ranges repeat across VPCs;
//! 2. an endpoint of any OTHER kind carries no CIDR, so it reports no target at all;
//! 3. the report PAGES by `MaxResults`/`NextToken`: following the token walks every target
//!    exactly once in address order, the last page carries no token, and a token minted while
//!    paging one endpoint is refused for another;
//! 4. an endpoint id the account holds no endpoint for is refused HTTP 400
//!    `InvalidVerifiedAccessEndpointId.NotFound`, and a `DryRun` reports the outcome the
//!    request would have had instead of any target;
//! 5. the report is a READ of the state as it stands: two identical calls answer identically,
//!    and an address taken inside the CIDR after the first call shows up as a target in the
//!    next one.
//!
//! Each table row is a distinct input along what the rule generalises over — which prefix
//! length, which base address, how many of the held addresses fall inside the block, which
//! endpoint kind, which page size — so no row is the specification. What a row EXPECTS is
//! computed here from the seeded addresses and the block itself ([`expected_addresses`]),
//! independently of the service, and the kind catalog is the model's own
//! `VERIFIED_ACCESS_ENDPOINT_TYPE_VALUES`, so every published kind is covered.

use ec2::ops::types::{
    VerifiedAccessGroup, VerifiedAccessInstance, VerifiedAccessInstanceCustomSubDomain,
    VERIFIED_ACCESS_ENDPOINT_TYPE_VALUES,
};
use ec2::state::{
    availability_zones_in_region, ComAmazonawsEc2Amazonec2State as Ec2State,
    NetworkInterfaceSpec, SecurityGroupSpec, SubnetSpec, VerifiedAccessGroupRecord,
    VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR, VERIFIED_ACCESS_ENDPOINT_TYPE_LOAD_BALANCER,
    VERIFIED_ACCESS_ENDPOINT_TYPE_NETWORK_INTERFACE, VERIFIED_ACCESS_ENDPOINT_TYPE_RDS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed, member, unescaped};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The custom subdomain the seeded Verified Access instance carries: the name the network
/// CIDR endpoints of that instance are reached under.
const SUB_DOMAIN: &str = "verified.example.test";

/// The IPv4 block both seeded VPCs are given — the SAME block on purpose, so an address held
/// in the other VPC is inside the endpoint's CIDR and still no target of it.
const VPC_CIDR: &str = "10.0.0.0/16";

/// The addresses interfaces of the endpoint's OWN VPC hold, each on an interface of its own.
/// `10.0.1.9` and `10.0.1.10` are both here because they sort the other way round as text
/// than as addresses, so the reported order is an address order rather than a text one.
const HELD_ADDRESSES: [&str; 4] = ["10.0.1.5", "10.0.1.6", "10.0.1.9", "10.0.1.10"];

/// A SECONDARY address of one more interface of that VPC: a secondary address is an address
/// the account holds just as much as a primary one.
const HELD_SECONDARY_ADDRESS: &str = "10.0.1.12";

/// The PRIMARY address of that interface, outside the narrow blocks the table names but
/// inside the VPC's own block — so a row whose CIDR is the whole VPC reports it too.
const HELD_PRIMARY_ADDRESS: &str = "10.0.9.9";

/// An address held in the OTHER VPC, inside the blocks the table names: the row that proves
/// an address of another VPC is no target of this endpoint.
const OTHER_VPC_ADDRESS: &str = "10.0.1.7";

/// An address nothing holds when the suite starts, taken later by
/// [`asking_again_reports_an_address_taken_since`].
const LATER_ADDRESS: &str = "10.0.1.11";

/// A well-formed Verified Access endpoint id this store holds no endpoint for.
const UNKNOWN_ENDPOINT_ID: &str = "vae-0123456789abcdef0";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// What the seeded store holds, for the tables to name rows by.
struct Fixture {
    /// The group on the instance that carries [`SUB_DOMAIN`].
    group_id: String,
    /// A group whose instance carries NO custom subdomain.
    plain_group_id: String,
    /// A security group the endpoint is associated with.
    security_group_id: String,
    /// The subnet of the endpoint's own VPC that its options block names.
    subnet_id: String,
    /// The subnet of that VPC the held addresses sit in.
    target_subnet_id: String,
    /// That VPC.
    vpc_id: String,
}

/// Stand up an emulator and seed the resources the endpoint references and the addresses its
/// targets are found among, through the same insert seams the creating operations land on.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, Fixture) {
    let (router, engine) = super::ec2_query::emulator();
    let fixture = seed(&engine);
    (router, engine, fixture)
}

/// Write the referenced resources into the engine's store and report the ids they were given.
fn seed(engine: &engine::InMemoryEngine) -> Fixture {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let zones = availability_zones_in_region(REGION);

    let vpc_id = guard.insert_vpc(VPC_CIDR, false, "default", "available", REGION, Vec::new());
    let subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.0.0/24".to_string(),
        availability_zone: zones[0].zone_name.clone(),
        availability_zone_id: zones[0].zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let target_subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.1.0/24".to_string(),
        availability_zone: zones[0].zone_name.clone(),
        availability_zone_id: zones[0].zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let security_group_id = guard.insert_security_group(SecurityGroupSpec {
        vpc_id: vpc_id.clone(),
        group_name: "verified-access".to_string(),
        description: "reaches the application".to_string(),
        region: REGION.to_string(),
        tags: Vec::new(),
        ip_permissions: Vec::new(),
        ip_permissions_egress: Vec::new(),
    });

    // One interface per held address, so each address is a resource of the account in its own
    // right rather than a second address of one interface …
    for address in HELD_ADDRESSES {
        take_address(&mut guard, &vpc_id, &target_subnet_id, &[address]);
    }
    // … and one interface whose PRIMARY address is outside the narrow blocks and whose
    // SECONDARY address is inside them.
    take_address(
        &mut guard,
        &vpc_id,
        &target_subnet_id,
        &[HELD_PRIMARY_ADDRESS, HELD_SECONDARY_ADDRESS],
    );

    // The other VPC, on the SAME block, holding an address inside the endpoint's CIDR.
    let other_vpc_id =
        guard.insert_vpc(VPC_CIDR, false, "default", "available", REGION, Vec::new());
    let other_subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: other_vpc_id.clone(),
        cidr_block: "10.0.1.0/24".to_string(),
        availability_zone: zones[0].zone_name.clone(),
        availability_zone_id: zones[0].zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    take_address(
        &mut guard,
        &other_vpc_id,
        &other_subnet_id,
        &[OTHER_VPC_ADDRESS],
    );

    let instance_id = guard.insert_verified_access_instance(
        VerifiedAccessInstance {
            description: Some("the instance whose CIDR endpoints have a subdomain".to_string()),
            cidr_endpoints_custom_sub_domain: Some(VerifiedAccessInstanceCustomSubDomain {
                sub_domain: Some(SUB_DOMAIN.to_string()),
                nameservers: Some(Vec::new()),
            }),
            ..VerifiedAccessInstance::default()
        },
        None,
    );
    let plain_instance_id = guard.insert_verified_access_instance(
        VerifiedAccessInstance {
            description: Some("the instance with no custom subdomain".to_string()),
            ..VerifiedAccessInstance::default()
        },
        None,
    );
    let group_id = guard.insert_verified_access_group(
        VerifiedAccessGroupRecord {
            view: VerifiedAccessGroup {
                verified_access_instance_id: Some(instance_id),
                description: Some("the group under test".to_string()),
                ..VerifiedAccessGroup::default()
            },
            ..VerifiedAccessGroupRecord::default()
        },
        REGION,
        None,
    );
    let plain_group_id = guard.insert_verified_access_group(
        VerifiedAccessGroupRecord {
            view: VerifiedAccessGroup {
                verified_access_instance_id: Some(plain_instance_id),
                description: Some("a group on an instance with no subdomain".to_string()),
                ..VerifiedAccessGroup::default()
            },
            ..VerifiedAccessGroupRecord::default()
        },
        REGION,
        None,
    );

    Fixture {
        group_id,
        plain_group_id,
        security_group_id,
        subnet_id,
        target_subnet_id,
        vpc_id,
    }
}

/// Give one network interface of `vpc_id` the addresses `addresses`, primary first, through
/// the insert seam `CreateNetworkInterface` itself lands on.
fn take_address(
    guard: &mut ec2::state::ComAmazonawsEc2Amazonec2State,
    vpc_id: &str,
    subnet_id: &str,
    addresses: &[&str],
) -> String {
    let zones = availability_zones_in_region(REGION);
    guard
        .insert_network_interface(NetworkInterfaceSpec {
            subnet_id: subnet_id.to_string(),
            vpc_id: vpc_id.to_string(),
            availability_zone: zones[0].zone_name.clone(),
            availability_zone_id: zones[0].zone_id.clone(),
            region: REGION.to_string(),
            description: format!("holds {addresses:?}"),
            interface_type: "interface".to_string(),
            private_ip_addresses: addresses.iter().map(|value| (*value).to_string()).collect(),
            groups: Vec::new(),
            attachment: None,
            requester: None,
            tags: Vec::new(),
            ..Default::default()
        })
        .expect("a seeded network interface")
}

/// The parameters of a create: the required members plus the options block of `kind`, filled
/// from `fixture`. A `cidr` endpoint's block names `cidr`.
fn create_params(kind: &str, group_id: &str, cidr: &str, fixture: &Fixture) -> Params {
    let mut params = vec![
        p("VerifiedAccessGroupId", group_id.to_string()),
        p("EndpointType", kind),
        p("AttachmentType", "vpc"),
    ];
    params.extend(indexed("SecurityGroupId", &[fixture.security_group_id.as_str()]));
    match kind {
        VERIFIED_ACCESS_ENDPOINT_TYPE_LOAD_BALANCER => {
            params.extend([
                p("LoadBalancerOptions.Protocol", "https"),
                p("LoadBalancerOptions.Port", "443"),
                p(
                    "LoadBalancerOptions.LoadBalancerArn",
                    "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/app/web/0a1b",
                ),
            ]);
            params.extend(indexed(
                "LoadBalancerOptions.SubnetId",
                &[fixture.subnet_id.as_str()],
            ));
        }
        VERIFIED_ACCESS_ENDPOINT_TYPE_NETWORK_INTERFACE => params.extend([
            p("NetworkInterfaceOptions.Protocol", "http"),
            p("NetworkInterfaceOptions.Port", "80"),
        ]),
        VERIFIED_ACCESS_ENDPOINT_TYPE_RDS => {
            params.extend([
                p("RdsOptions.Protocol", "tcp"),
                p("RdsOptions.Port", "5432"),
                p("RdsOptions.RdsEndpoint", "ledger.example.test"),
            ]);
            params.extend(indexed("RdsOptions.SubnetId", &[fixture.subnet_id.as_str()]));
        }
        // `cidr`, the kind whose targets this suite is about.
        _ => {
            params.extend([
                p("CidrOptions.Protocol", "tcp"),
                p("CidrOptions.Cidr", cidr),
                p("CidrOptions.PortRange.1.FromPort", "443"),
                p("CidrOptions.PortRange.1.ToPort", "443"),
            ]);
            params.extend(indexed("CidrOptions.SubnetId", &[fixture.subnet_id.as_str()]));
        }
    }
    params
}

/// Create one endpoint of `kind` that must succeed, and answer the id it was minted together
/// with the endpoint domain the service generated for it.
async fn seed_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    kind: &str,
    group_id: &str,
    cidr: &str,
    fixture: &Fixture,
) -> (String, String) {
    let (status, body) = call(
        router,
        REGION,
        "CreateVerifiedAccessEndpoint",
        &create_params(kind, group_id, cidr, fixture),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessEndpoint, got {status} \
         with body {body}"
    );
    let endpoint_id = element(&body, "verifiedAccessEndpointId").unwrap_or_default();
    assert!(
        endpoint_id.starts_with("vae-"),
        "case {case:?}: expected a minted endpoint id, got {endpoint_id:?}"
    );
    let endpoint_domain = element(&body, "endpointDomain").unwrap_or_default();
    assert!(
        !endpoint_domain.is_empty(),
        "case {case:?}: expected a generated endpoint domain, got {body}"
    );
    (endpoint_id, endpoint_domain)
}

/// One `GetVerifiedAccessEndpointTargets` call.
async fn targets(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "GetVerifiedAccessEndpointTargets", params).await
}

/// The call that must succeed, as `(the reported targets, the next token)`.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> (Vec<Target>, Option<String>) {
    let (status, body) = targets(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetVerifiedAccessEndpointTargets, got \
         {status} with body {body}"
    );
    (reported_targets(case, &body), member(&body, "nextToken"))
}

/// One reported target: `(the endpoint it belongs to, its IP address, its DNS name)`.
type Target = (String, String, String);

/// The targets a response reports, in the order it reported them. Read through the XML tree,
/// so a member is never read out of a shape that merely shares its name.
fn reported_targets(case: &str, body: &str) -> Vec<Target> {
    let parsed = super::wire::document(case, body);
    super::wire::set_items(parsed.root_element(), "verifiedAccessEndpointTargetSet")
        .into_iter()
        .map(|item| {
            let text = |name: &str| super::wire::child_text(item, name).unwrap_or_default();
            (
                text("verifiedAccessEndpointId"),
                text("verifiedAccessEndpointTargetIpAddress"),
                unescaped(&text("verifiedAccessEndpointTargetDns")),
            )
        })
        .collect()
}

/// Every address the endpoint's OWN VPC holds, as the suite seeded them.
fn held_addresses() -> Vec<String> {
    HELD_ADDRESSES
        .iter()
        .chain([&HELD_SECONDARY_ADDRESS, &HELD_PRIMARY_ADDRESS])
        .map(|address| (*address).to_string())
        .collect()
}

/// The addresses a `cidr` endpoint over `cidr` must report as targets: the seeded addresses of
/// its own VPC that lie inside the block, in address order. Computed here from the block
/// itself rather than asked of the service.
fn expected_addresses(cidr: &str) -> Vec<String> {
    let mut inside: Vec<u32> = held_addresses()
        .iter()
        .filter_map(|address| address.parse::<std::net::Ipv4Addr>().ok())
        .map(u32::from)
        .filter(|address| contains(cidr, *address))
        .collect();
    inside.sort_unstable();
    inside
        .into_iter()
        .map(|address| std::net::Ipv4Addr::from(address).to_string())
        .collect()
}

/// Whether the IPv4 block `cidr` covers the address `address`.
fn contains(cidr: &str, address: u32) -> bool {
    let (base, prefix_length) = cidr.split_once('/').expect("a CIDR block");
    let base = u32::from(base.parse::<std::net::Ipv4Addr>().expect("an IPv4 address"));
    let prefix_length: u32 = prefix_length.parse().expect("a prefix length");
    let span = u32::MAX.checked_shr(prefix_length).unwrap_or_default();
    (base & !span) == (address & !span)
}

/// The DNS name a target carries for `address` under `zone`: the address with the separators
/// a DNS label cannot carry written as hyphens, inside that zone.
fn target_dns(address: &str, zone: &str) -> String {
    format!("{}.{zone}", address.replace('.', "-"))
}

/// The targets expected of `endpoint_id` over `cidr`, with every member spelled out.
fn expected_targets(endpoint_id: &str, cidr: &str, zone: &str) -> Vec<Target> {
    expected_addresses(cidr)
        .into_iter()
        .map(|address| {
            let dns = target_dns(&address, zone);
            (endpoint_id.to_string(), address, dns)
        })
        .collect()
}

/// RULE 1 over the blocks: only the addresses the account really holds inside the endpoint's
/// CIDR, in its own VPC, are reported — one target each, in address order, naming the
/// endpoint, the address and that address's DNS name inside the instance's custom subdomain.
#[tokio::test]
async fn only_addresses_held_inside_the_cidr_are_reported() {
    // Rows differ along the prefix length and the base address, so they differ in HOW MANY of
    // the held addresses fall inside: a /32 on one of them, a /30 holding two, a /28 holding
    // the five inside `10.0.1.0`, the subnet's own /24, the whole VPC, and a block of the VPC
    // nothing occupies at all.
    let rows: [&str; 6] = [
        "10.0.1.5/32",
        "10.0.1.4/30",
        "10.0.1.0/28",
        "10.0.1.0/24",
        VPC_CIDR,
        "10.0.8.0/24",
    ];
    for cidr in rows {
        let (router, _engine, fixture) = emulator();
        let case = format!("cidr {cidr}");
        let (endpoint_id, _domain) = seed_endpoint(
            &router,
            &case,
            VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
            &fixture.group_id,
            cidr,
            &fixture,
        )
        .await;

        let (reported_set, next_token) = reported(
            &router,
            &case,
            &[p("VerifiedAccessEndpointId", endpoint_id.clone())],
        )
        .await;
        assert_eq!(
            reported_set,
            expected_targets(&endpoint_id, cidr, SUB_DOMAIN),
            "case {case:?}: expected one target per held address inside the block, got \
             {reported_set:?}"
        );
        assert!(
            !reported_set
                .iter()
                .any(|(_, address, _)| address == OTHER_VPC_ADDRESS),
            "case {case:?}: {OTHER_VPC_ADDRESS} is held in ANOTHER VPC, so it is no target \
             of this endpoint, got {reported_set:?}"
        );
        assert!(
            next_token.is_none(),
            "case {case:?}: nothing follows the whole set, so no token is due, got \
             {next_token:?}"
        );
    }
}

/// RULE 1's fallback: an endpoint whose instance carries NO custom subdomain names its
/// targets under the endpoint's own generated domain instead.
#[tokio::test]
async fn targets_fall_back_to_the_endpoints_own_domain() {
    let (router, _engine, fixture) = emulator();
    let case = "an instance with no custom subdomain";
    let cidr = "10.0.1.0/28";
    let (endpoint_id, endpoint_domain) = seed_endpoint(
        &router,
        case,
        VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
        &fixture.plain_group_id,
        cidr,
        &fixture,
    )
    .await;

    let (reported_set, _next_token) = reported(
        &router,
        case,
        &[p("VerifiedAccessEndpointId", endpoint_id.clone())],
    )
    .await;
    assert_eq!(
        reported_set,
        expected_targets(&endpoint_id, cidr, &endpoint_domain),
        "case {case:?}: expected the targets under {endpoint_domain:?}, got {reported_set:?}"
    );
}

/// RULE 2 over the kind catalog: a kind of endpoint that carries no CIDR reports no target,
/// while the `cidr` kind of the same catalog reports the held addresses — so an empty set is
/// the ANSWER for those kinds rather than this operation reporting nothing for anybody.
#[tokio::test]
async fn an_endpoint_that_carries_no_cidr_reports_no_target() {
    let cidr = "10.0.1.0/28";
    for kind in VERIFIED_ACCESS_ENDPOINT_TYPE_VALUES.iter().copied() {
        let (router, _engine, fixture) = emulator();
        let case = format!("kind {kind}");
        let (endpoint_id, _domain) = seed_endpoint(
            &router,
            &case,
            kind,
            &fixture.group_id,
            cidr,
            &fixture,
        )
        .await;

        let (reported_set, next_token) =
            reported(&router, &case, &[p("VerifiedAccessEndpointId", endpoint_id)]).await;
        let expected = if kind == VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR {
            expected_addresses(cidr).len()
        } else {
            0
        };
        assert_eq!(
            reported_set.len(),
            expected,
            "case {case:?}: expected {expected} targets, got {reported_set:?}"
        );
        assert!(
            next_token.is_none(),
            "case {case:?}: nothing follows the set, so no token is due, got {next_token:?}"
        );
    }
}

/// RULE 3 over the page sizes: following `NextToken` reports every target exactly once in
/// address order, no page holds more than `MaxResults`, and the last page carries no token.
#[tokio::test]
async fn following_the_token_walks_every_target_exactly_once() {
    // The whole VPC's block holds every seeded address, so the page sizes below — the model's
    // own minimum, which does not divide the set, and a size past it — walk it in two pages
    // and in one.
    let cidr = VPC_CIDR;
    for page_size in [5usize, 1000] {
        let (router, _engine, fixture) = emulator();
        let case = format!("page size {page_size}");
        let (endpoint_id, _domain) = seed_endpoint(
            &router,
            &case,
            VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
            &fixture.group_id,
            cidr,
            &fixture,
        )
        .await;

        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // A bound on the walk, so a token that never advances fails the case instead of
        // looping: one page per target, plus the page a token could point past.
        let most_pages = expected_addresses(cidr).len() + 2;
        for page in 0..most_pages {
            let mut params = vec![
                p("VerifiedAccessEndpointId", endpoint_id.clone()),
                p("MaxResults", page_size.to_string()),
            ];
            if let Some(token) = &token {
                params.push(p("NextToken", token.clone()));
            }
            let (reported_set, next_token) = reported(&router, &case, &params).await;
            assert!(
                reported_set.len() <= page_size,
                "case {case:?}: page {page} holds {} targets, above the {page_size} asked \
                 for",
                reported_set.len()
            );
            walked.extend(reported_set.iter().map(|(_, address, _)| address.clone()));
            token = next_token;
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the walk must end without a token, got {token:?}"
        );
        assert_eq!(
            walked,
            expected_addresses(cidr),
            "case {case:?}: expected every held address walked once in address order, got \
             {walked:?}"
        );
    }
}

/// RULE 3's scope: a token minted while paging ONE endpoint names no position in another
/// endpoint's walk, and neither does a value this service never minted.
#[tokio::test]
async fn a_token_from_another_walk_is_refused() {
    let (router, _engine, fixture) = emulator();
    let case = "a token from another endpoint";
    let (first_id, _first_domain) = seed_endpoint(
        &router,
        case,
        VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
        &fixture.group_id,
        VPC_CIDR,
        &fixture,
    )
    .await;
    let (second_id, _second_domain) = seed_endpoint(
        &router,
        case,
        VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
        &fixture.group_id,
        VPC_CIDR,
        &fixture,
    )
    .await;

    let (first_page, token) = reported(
        &router,
        case,
        &[
            p("VerifiedAccessEndpointId", first_id.clone()),
            p("MaxResults", "5"),
        ],
    )
    .await;
    assert_eq!(
        first_page.len(),
        5,
        "case {case:?}: expected the first page to hold the 5 asked for, got {first_page:?}"
    );
    let token = token.expect("a page of 5 out of 6 held addresses carries a token");

    // The same token is good for the walk it was minted in …
    let (resumed, resumed_token) = reported(
        &router,
        case,
        &[
            p("VerifiedAccessEndpointId", first_id),
            p("MaxResults", "5"),
            p("NextToken", token.clone()),
        ],
    )
    .await;
    assert_eq!(
        resumed.len(),
        expected_addresses(VPC_CIDR).len() - 5,
        "case {case:?}: expected the rest of the first walk, got {resumed:?}"
    );
    assert!(
        resumed_token.is_none(),
        "case {case:?}: the second page ends the walk, got {resumed_token:?}"
    );

    // … and for no other, including a value nothing minted.
    for (label, presented) in [
        ("another endpoint's walk", token.as_str()),
        ("a token nothing minted", "not-a-token"),
    ] {
        let (status, body) = targets(
            &router,
            &[
                p("VerifiedAccessEndpointId", second_id.clone()),
                p("NextToken", presented),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400 for a token of another walk, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidPaginationToken",
            "case {label:?}: expected InvalidPaginationToken, got {body}"
        );
    }
}

/// RULE 4: an id this account holds no endpoint for is refused with that resource's own
/// not-found code and sentence, and a `DryRun` of a request that would have succeeded is
/// answered the permitted side of the probe rather than with any target.
#[tokio::test]
async fn an_unknown_endpoint_is_refused_and_a_dry_run_reports_nothing() {
    let (router, _engine, fixture) = emulator();
    let case = "an unknown endpoint";
    let (endpoint_id, _domain) = seed_endpoint(
        &router,
        case,
        VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
        &fixture.group_id,
        "10.0.1.0/28",
        &fixture,
    )
    .await;

    for presented in [UNKNOWN_ENDPOINT_ID, "vae-00000000", "vae-not-an-id"] {
        let (status, body) = targets(&router, &[p("VerifiedAccessEndpointId", presented)]).await;
        assert_eq!(
            status, 400,
            "case {presented:?}: expected HTTP 400 for an id naming no endpoint, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessEndpointId.NotFound",
            "case {presented:?}: expected the endpoint's own not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The verified-access-endpoint ID '{presented}' does not exist"),
            "case {presented:?}: expected the refusal to name the id the request gave, got \
             {body}"
        );
    }

    let (status, body) = targets(
        &router,
        &[
            p("VerifiedAccessEndpointId", endpoint_id),
            p("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected HTTP 412 from the dry run, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert!(
        reported_targets(case, &body).is_empty(),
        "case {case:?}: a dry run reports no target, got {body}"
    );
}

/// RULE 5: the report is a read of the state as it stands — asking twice answers the same
/// set, and an address taken inside the CIDR between two calls is a target in the second.
#[tokio::test]
async fn asking_again_reports_an_address_taken_since() {
    let (router, engine, fixture) = emulator();
    let case = "an address taken since the last read";
    let cidr = "10.0.1.0/28";
    let (endpoint_id, _domain) = seed_endpoint(
        &router,
        case,
        VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
        &fixture.group_id,
        cidr,
        &fixture,
    )
    .await;

    let params = [p("VerifiedAccessEndpointId", endpoint_id.clone())];
    let (first, first_token) = reported(&router, case, &params).await;
    let (second, second_token) = reported(&router, case, &params).await;
    assert_eq!(
        first, second,
        "case {case:?}: expected the second read to report {first:?}, got {second:?}"
    );
    assert_eq!(
        (first_token, second_token),
        (None, None),
        "case {case:?}: the whole set fits in one page, so neither read is paged"
    );

    // Something takes one more address of the block …
    take_address(
        &mut engine.state::<Ec2State>().lock(),
        &fixture.vpc_id,
        &fixture.target_subnet_id,
        &[LATER_ADDRESS],
    );
    // … and the next read reports it, in its place in the address order.
    let (after, _after_token) = reported(&router, case, &params).await;
    let mut expected: Vec<String> = first
        .iter()
        .map(|(_, address, _)| address.clone())
        .collect();
    expected.push(LATER_ADDRESS.to_string());
    expected.sort_by_key(|address| {
        u32::from(
            address
                .parse::<std::net::Ipv4Addr>()
                .expect("an IPv4 address"),
        )
    });
    assert_eq!(
        after
            .iter()
            .map(|(_, address, _)| address.clone())
            .collect::<Vec<String>>(),
        expected,
        "case {case:?}: expected {LATER_ADDRESS} reported beside the addresses already held, \
         got {after:?}"
    );
}
