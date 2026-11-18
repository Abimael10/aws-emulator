// SPDX-License-Identifier: CC0-1.0

//! The network interfaces a directory's servers answer on, driven end to end over HTTP with
//! the owner of network interfaces mounted beside this service.
//!
//! RULE: a create that places a directory's servers in the caller's subnets puts ONE elastic
//! network interface per resolved subnet in the service that OWNS network interfaces — in that
//! subnet, carrying the security group this service provisioned for those servers, naming the
//! directory in its description and holding an address of that subnet's own block — and the
//! teardown `DeleteDirectory` starts releases every one of them there, after which the owner
//! answers for each released identifier the way it answers for one it holds nothing under.
//! `DirectoryVpcSettings$SubnetIds`' own documentation is what places them: "Directory Service
//! creates a directory server and a DNS server in each of these subnets", and
//! `DirectoryConnectSettings$SubnetIds` places an AD Connector's servers the same way.
//!
//! The rows differ along what the rule generalises over: the create that places the servers
//! and so the directory type (Managed Microsoft AD, Simple AD, AD Connector), the blocks of
//! the chosen subnets (`/24`, `/28`, `/20`), the order the create names them in, whether they
//! are a client's own subnets or ones EC2 materialises for the account's default VPC, and
//! whether the account has them at all.
//!
//! A network interface is an Amazon EC2 resource, so EC2 is mounted beside this service on ONE
//! engine (`crate::directory_dns_addresses::network`, the account network this crate's
//! placement suites share) and every assertion is READ BACK from EC2 itself: an interface this
//! service only recorded would be in no listing of the owner's and could not pass, and one it
//! only pretended to delete would still be there. Nothing reaches into either store.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use aws_sdk_ec2::error::ProvideErrorMetadata as _;
use aws_sdk_ec2::types::NetworkInterface;
use ipnet::Ipv4Net;
use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::ad_connector::connect_directory;
use crate::directory_dns_addresses::{ec2_client, network, AccountSubnet, ABSENT_SUBNET};
use crate::directory_lifecycle::{
    create_directory, descriptions, ok, string, PASSWORD, STAGE_ACTIVE,
};
use crate::directory_security_group::vpc_of;
use crate::microsoft_ad::create_microsoft_ad;

/// How long a polled read may take to report the directory in service before a case fails:
/// far longer than any span this service publishes, and well inside what a client's waiter
/// allows, so a loaded or faster host changes nothing.
const POLL_DEADLINE: Duration = Duration::from_secs(20);

/// How often such a read asks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `CustomerDnsIps` an AD Connector forwards to: addresses of a self-managed network this
/// emulator does not hold, which is what a connector's create names.
const CUSTOMER_DNS_IPS: [&str; 2] = ["10.9.0.10", "10.9.1.10"];

/// EC2's own code for an identifier it holds no network interface under
/// (`InvalidNetworkInterfaceID.NotFound`), which is what a released interface's identifier
/// must answer with.
const INTERFACE_NOT_FOUND: &str = "InvalidNetworkInterfaceID.NotFound";

/// EC2's own code for an identifier it holds no security group under (`InvalidGroup.NotFound`).
const GROUP_NOT_FOUND: &str = "InvalidGroup.NotFound";

/// Every network interface of the account EC2 reports, read through its own
/// `DescribeNetworkInterfaces`.
async fn interfaces_held_by_ec2(ec2: &aws_sdk_ec2::Client, case: &str) -> Vec<NetworkInterface> {
    ec2.describe_network_interfaces()
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the account's network interfaces: {error:?}")
        })
        .network_interfaces()
        .to_vec()
}

/// The interfaces of the account that belong to the directory `directory_id`: the ones whose
/// description names it, which is how a client reconciling a directory against the owner of
/// network interfaces finds them.
async fn directory_interfaces(
    ec2: &aws_sdk_ec2::Client,
    directory_id: &str,
    case: &str,
) -> Vec<NetworkInterface> {
    interfaces_held_by_ec2(ec2, case)
        .await
        .into_iter()
        .filter(|interface| {
            interface
                .description()
                .is_some_and(|description| description.contains(directory_id))
        })
        .collect()
}

/// The subnets `interfaces` sit in, as a set compares them.
fn subnets_of(interfaces: &[NetworkInterface]) -> BTreeSet<String> {
    interfaces
        .iter()
        .filter_map(|interface| interface.subnet_id().map(str::to_string))
        .collect()
}

/// The security groups one interface carries, by identifier.
fn groups_of(interface: &NetworkInterface) -> BTreeSet<String> {
    interface
        .groups()
        .iter()
        .filter_map(|group| group.group_id().map(str::to_string))
        .collect()
}

/// The primary private addresses of `interfaces`, as a set compares them.
fn addresses_of(interfaces: &[NetworkInterface]) -> BTreeSet<String> {
    interfaces
        .iter()
        .filter_map(|interface| interface.private_ip_address().map(str::to_string))
        .collect()
}

/// The subnet of `subnets` that `subnet_id` names, so a case can assert an interface against
/// the block the OWNER of subnets reported for the very subnet it sits in.
fn chosen_subnet<'a>(subnets: &'a [AccountSubnet], subnet_id: &str, case: &str) -> &'a AccountSubnet {
    subnets
        .iter()
        .find(|subnet| subnet.id == subnet_id)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {subnet_id} among the chosen subnets {subnets:?}")
        })
}

/// The one description of `directory_id`, or `None` once the teardown has taken the record
/// away.
async fn described_if_held(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Option<Value> {
    let answer = ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    let page = descriptions(&answer, case);
    assert!(
        page.len() <= 1,
        "case {case:?}: expected at most one description of {directory_id}, got {page:?}"
    );
    page.first().cloned()
}

/// The directory `directory_id`, polled through `DescribeDirectories` until it reports itself
/// in service, the way a client with a waiter does.
async fn described_in_service(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    let started = Instant::now();
    loop {
        let described = described_if_held(router, directory_id, case)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected {directory_id} to be described"));
        if described["Stage"] == json!(STAGE_ACTIVE) {
            return described;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected {directory_id} to report itself in service within \
             {POLL_DEADLINE:?}, last observed {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `DnsIpAddrs` a description reports, as a set compares them.
fn dns_addresses(described: &Value, case: &str) -> BTreeSet<String> {
    described["DnsIpAddrs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a DnsIpAddrs list, got {described}"))
        .iter()
        .map(|address| {
            address
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an address, got {described}"))
                .to_string()
        })
        .collect()
}

/// The identifiers of `subnets`, in the order a create names them.
fn subnet_ids(subnets: &[AccountSubnet]) -> Vec<String> {
    subnets.iter().map(|subnet| subnet.id.clone()).collect()
}

/// Which create a row drives, and so which placement member the directory reports its network
/// in and whether it serves DNS from the servers this service placed.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `CreateMicrosoftAD`, reported as `VpcSettings`; serves DNS from its own controllers.
    MicrosoftAd,
    /// `CreateDirectory` (Simple AD), reported as `VpcSettings`; serves DNS from its own
    /// servers.
    SimpleAd,
    /// `ConnectDirectory` (AD Connector), reported as `ConnectSettings`; serves DNS from the
    /// self-managed directory it connects to, not from the servers placed here.
    Connector,
}

impl Kind {
    /// The member of the description this kind of directory reports its placement — and so the
    /// group its interfaces carry — in.
    fn placement(self) -> &'static str {
        match self {
            Kind::MicrosoftAd | Kind::SimpleAd => "VpcSettings",
            Kind::Connector => "ConnectSettings",
        }
    }

    /// Whether the addresses this kind of directory reports as its DNS servers' are the
    /// addresses of the servers placed in the chosen subnets.
    fn serves_dns_from_its_own_servers(self) -> bool {
        match self {
            Kind::MicrosoftAd | Kind::SimpleAd => true,
            Kind::Connector => false,
        }
    }
}

/// Create a directory of `kind` named `name`, whose settings block names `vpc_id` and places
/// its servers in `subnets`, and return its id.
async fn create_in(
    router: &MultiServiceRouter,
    kind: Kind,
    name: &str,
    vpc_id: &str,
    subnets: &[AccountSubnet],
    case: &str,
) -> String {
    let ids = subnet_ids(subnets);
    let body = match kind {
        Kind::MicrosoftAd => json!({
            "Name": name,
            "Password": PASSWORD,
            "VpcSettings": { "VpcId": vpc_id, "SubnetIds": ids }
        }),
        Kind::SimpleAd => json!({
            "Name": name,
            "Password": PASSWORD,
            "Size": "Small",
            "VpcSettings": { "VpcId": vpc_id, "SubnetIds": ids }
        }),
        Kind::Connector => json!({
            "Name": name,
            "Password": PASSWORD,
            "Size": "Small",
            "ConnectSettings": {
                "VpcId": vpc_id,
                "SubnetIds": ids,
                "CustomerDnsIps": CUSTOMER_DNS_IPS,
                "CustomerUserName": "ad-connector-svc",
            }
        }),
    };
    match kind {
        Kind::MicrosoftAd => create_microsoft_ad(router, body, case).await,
        Kind::SimpleAd => create_directory(router, body, case).await,
        Kind::Connector => connect_directory(router, body, case).await,
    }
}

/// `DeleteDirectory`, asserting it answers 200 with the id it was asked about.
async fn delete(router: &MultiServiceRouter, directory_id: &str, case: &str) {
    let answer = ok(
        router,
        "DeleteDirectory",
        json!({ "DirectoryId": directory_id }),
        case,
    )
    .await;
    assert_eq!(
        answer["DirectoryId"],
        json!(directory_id),
        "case {case:?}: expected the delete to report the id it was asked about, got {answer}"
    );
}

/// RULE: every directory this service places in a VPC holds one network interface per chosen
/// subnet in the service that owns network interfaces — in that subnet, in that subnet's VPC,
/// carrying the group provisioned for its servers, naming the directory, and holding an
/// address of that subnet's own block.
#[tokio::test]
async fn a_directory_placed_in_a_vpc_holds_one_interface_per_subnet_in_ec2() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));

    // The rows differ in the create that places the servers, in the blocks of the chosen
    // subnets, in the order the create names them, and in whether the subnets are the client's
    // own or the account's default ones.
    let rows: [(&str, Kind, Vec<AccountSubnet>); 5] = [
        (
            "a Managed Microsoft AD in two /24 subnets",
            Kind::MicrosoftAd,
            vec![network.client[0].clone(), network.client[1].clone()],
        ),
        (
            "a Managed Microsoft AD in a /28 and a /20, named in that order",
            Kind::MicrosoftAd,
            vec![network.client[2].clone(), network.client[3].clone()],
        ),
        (
            "a Managed Microsoft AD in the account's default subnets",
            Kind::MicrosoftAd,
            vec![network.default[0].clone(), network.default[1].clone()],
        ),
        (
            "a Simple AD in two /24 subnets, named in the other order",
            Kind::SimpleAd,
            vec![network.client[1].clone(), network.client[0].clone()],
        ),
        (
            "an AD Connector in a /20 and a /28",
            Kind::Connector,
            vec![network.client[3].clone(), network.client[2].clone()],
        ),
    ];

    let mut every_interface: BTreeSet<String> = BTreeSet::new();
    for (index, (case, kind, subnets)) in rows.iter().enumerate() {
        let name = format!("eni{index}.example.com");
        // The VPC a row names is the VPC its own subnets sit in, as EC2 reports it: a
        // directory's servers are placed in subnets OF the VPC the create names.
        let directory_id = create_in(router, *kind, &name, &subnets[0].vpc, subnets, case).await;

        // One interface per chosen subnet, each in its OWN subnet: the count and the set of
        // subnets together are what rule out two interfaces sharing one subnet.
        let interfaces = directory_interfaces(&ec2, &directory_id, case).await;
        let chosen: BTreeSet<String> = subnet_ids(subnets).into_iter().collect();
        assert_eq!(
            interfaces.len(),
            chosen.len(),
            "case {case:?}: expected one interface per chosen subnet ({chosen:?}) for \
             {directory_id}, got {interfaces:?}"
        );
        assert_eq!(
            subnets_of(&interfaces),
            chosen,
            "case {case:?}: expected the interfaces of {directory_id} in the chosen subnets, \
             got {interfaces:?}"
        );

        // Each one carries the group this service provisioned for the directory's servers, as
        // the directory itself reports it, and sits in the VPC the chosen subnets belong to.
        let described = described_in_service(router, &directory_id, case).await;
        let group_id = string(&described[kind.placement()], "SecurityGroupId", case);
        let expected_vpc = vpc_of(&ec2, &subnets[0].id).await;
        for interface in &interfaces {
            let groups = groups_of(interface);
            assert!(
                groups.contains(&group_id),
                "case {case:?}: expected the interface to carry the directory's group \
                 {group_id}, got {groups:?} on {interface:?}"
            );
            assert_eq!(
                interface.vpc_id(),
                Some(expected_vpc.as_str()),
                "case {case:?}: expected the interface in the VPC of the chosen subnets \
                 ({expected_vpc}), got {interface:?}"
            );
            assert!(
                interface
                    .description()
                    .is_some_and(|description| description.contains(&directory_id)),
                "case {case:?}: expected the interface's description to name {directory_id}, \
                 got {interface:?}"
            );

            // The address it holds is one of ITS OWN subnet's assignable addresses: a server
            // placed in a subnet is on that subnet's block and nowhere else. The block is the
            // one the OWNER of subnets reported for that very subnet.
            let subnet_id = interface
                .subnet_id()
                .unwrap_or_else(|| panic!("case {case:?}: expected a subnet on {interface:?}"));
            let cidr = &chosen_subnet(subnets, subnet_id, case).cidr;
            let block: Ipv4Net = cidr.parse().unwrap_or_else(|error| {
                panic!("case {case:?}: EC2 reported the block {cidr:?}: {error:?}")
            });
            let address: Ipv4Addr = interface
                .private_ip_address()
                .unwrap_or_else(|| panic!("case {case:?}: expected an address on {interface:?}"))
                .parse()
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: expected an IPv4 address on {interface:?}: {error:?}")
                });
            assert!(
                block.contains(&address) && address != block.network() && address != block.broadcast(),
                "case {case:?}: expected {address} inside the block {block} of {subnet_id}, got \
                 {interface:?}"
            );

            // No two directories — and no two servers — share an interface.
            let interface_id = interface
                .network_interface_id()
                .unwrap_or_else(|| panic!("case {case:?}: expected an id on {interface:?}"))
                .to_string();
            assert!(
                every_interface.insert(interface_id.clone()),
                "case {case:?}: expected an interface of this directory's own, got \
                 {interface_id} again"
            );
        }

        // A directory that serves DNS from the servers placed here answers at exactly the
        // addresses those servers' interfaces hold: the address this service reports and the
        // address the owner of network interfaces really holds are one value.
        if kind.serves_dns_from_its_own_servers() {
            assert_eq!(
                dns_addresses(&described, case),
                addresses_of(&interfaces),
                "case {case:?}: expected {directory_id} to serve DNS at the addresses its \
                 interfaces hold, got {described} against {interfaces:?}"
            );
        }
    }
}

/// RULE: the interfaces exist for the directory's servers, so they do not outlive the
/// directory: a `DeleteDirectory` releases every one of them in the service that owns network
/// interfaces — before the group they carry, which that owner refuses to delete while an
/// interface still holds it — so a client reconciling the account against EC2 afterwards finds
/// neither, and each released identifier is refused the way an identifier the owner holds
/// nothing under is. The network of every OTHER directory is untouched.
#[tokio::test]
async fn deleting_a_directory_releases_the_interfaces_ec2_held_for_its_servers() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let before: BTreeSet<String> = interfaces_held_by_ec2(&ec2, "before any directory")
        .await
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();

    // The rows differ in the create that placed the interfaces and in the subnets they sit in.
    let rows: [(&str, Kind, Vec<AccountSubnet>); 3] = [
        (
            "a Managed Microsoft AD",
            Kind::MicrosoftAd,
            vec![network.client[0].clone(), network.client[1].clone()],
        ),
        (
            "a Simple AD in a /28 and a /20",
            Kind::SimpleAd,
            vec![network.client[2].clone(), network.client[3].clone()],
        ),
        (
            "an AD Connector in two /24 subnets, named in the other order",
            Kind::Connector,
            vec![network.client[1].clone(), network.client[0].clone()],
        ),
    ];

    // Every row's directory is created before any of them is deleted, so each delete runs with
    // the OTHER directories' interfaces still in EC2: a teardown that took the account's
    // interfaces with it rather than its own could not pass.
    let mut held: Vec<(&str, String, Vec<String>, String)> = Vec::with_capacity(rows.len());
    for (index, (case, kind, subnets)) in rows.iter().enumerate() {
        let name = format!("enirelease{index}.example.com");
        let directory_id = create_in(router, *kind, &name, &network.vpc, subnets, case).await;
        let interfaces = directory_interfaces(&ec2, &directory_id, case).await;
        assert_eq!(
            interfaces.len(),
            subnets.len(),
            "case {case:?}: expected one interface per chosen subnet for {directory_id}, got \
             {interfaces:?}"
        );
        let interface_ids: Vec<String> = interfaces
            .iter()
            .filter_map(|interface| interface.network_interface_id().map(str::to_string))
            .collect();
        let described = described_if_held(router, &directory_id, case)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected {directory_id} to be described"));
        let group_id = string(&described[kind.placement()], "SecurityGroupId", case);
        held.push((case, directory_id, interface_ids, group_id));
    }

    for (position, (case, directory_id, interface_ids, group_id)) in held.iter().enumerate() {
        delete(router, directory_id, case).await;

        // Nothing of this directory is left in the owner of network interfaces...
        let remaining = directory_interfaces(&ec2, directory_id, case).await;
        assert!(
            remaining.is_empty(),
            "case {case:?}: expected EC2 to hold no interface of the deleted {directory_id}, \
             got {remaining:?}"
        );
        // ...and each released identifier is refused the way one the owner holds nothing under
        // is, which is what lets the caller take its own subnets away again.
        for interface_id in interface_ids {
            let refused = ec2
                .describe_network_interfaces()
                .network_interface_ids(interface_id)
                .send()
                .await
                .expect_err("a released network interface must not be described");
            assert_eq!(
                refused.code(),
                Some(INTERFACE_NOT_FOUND),
                "case {case:?}: expected {INTERFACE_NOT_FOUND} for the released \
                 {interface_id}, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                Some(format!("The networkInterface ID '{interface_id}' does not exist").as_str()),
                "case {case:?}: expected the not-found message to name {interface_id}, got \
                 {refused:?}"
            );
        }
        // The group those interfaces carried goes with them: the interfaces are released
        // first, so the group's own delete is not held by them.
        let refused = ec2
            .describe_security_groups()
            .group_ids(group_id)
            .send()
            .await
            .expect_err("a released security group must not be described");
        assert_eq!(
            refused.code(),
            Some(GROUP_NOT_FOUND),
            "case {case:?}: expected {GROUP_NOT_FOUND} for the released {group_id}, got \
             {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(format!("The security group '{group_id}' does not exist").as_str()),
            "case {case:?}: expected the not-found message to name {group_id}, got {refused:?}"
        );

        // Only this directory's network went: every directory not yet deleted still has its
        // interfaces.
        for (other_case, other_directory, other_interfaces, _) in held.iter().skip(position + 1) {
            let still_there = directory_interfaces(&ec2, other_directory, other_case).await;
            assert_eq!(
                still_there
                    .iter()
                    .filter_map(|interface| interface.network_interface_id().map(str::to_string))
                    .collect::<BTreeSet<String>>(),
                other_interfaces.iter().cloned().collect::<BTreeSet<String>>(),
                "case {case:?}: expected the interfaces of {other_directory} to survive the \
                 delete of {directory_id}, got {still_there:?}"
            );
        }

        // A repeated delete releases nothing twice and is still served.
        delete(router, directory_id, case).await;
    }

    // Nothing of the account's own network was taken by any of those teardowns.
    let after: BTreeSet<String> = interfaces_held_by_ec2(&ec2, "after every teardown")
        .await
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();
    assert_eq!(
        after, before,
        "case \"after every teardown\": expected the account to hold exactly the interfaces it \
         held before the directories"
    );
}

/// RULE: nothing is placed on a network the account does not have — a create whose subnets the
/// owner of subnets holds nothing for is refused, so no interface is created anywhere for it,
/// and a directory created with no placement at all holds none either and its teardown
/// releases nothing.
#[tokio::test]
async fn a_directory_whose_placement_does_not_resolve_holds_no_interface() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let before: BTreeSet<String> = interfaces_held_by_ec2(&ec2, "before any directory")
        .await
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();

    // A placement the account has no subnet for never becomes a directory: the create is
    // refused before any network is asked for, so EC2 holds exactly what it held.
    let unresolved = "a Managed Microsoft AD whose subnets the account has none of";
    let (status, parsed) = crate::directory_lifecycle::call(
        router,
        "CreateMicrosoftAD",
        json!({
            "Name": "noeni.example.com",
            "Password": PASSWORD,
            "VpcSettings": {
                "VpcId": network.vpc,
                "SubnetIds": [ABSENT_SUBNET, "subnet-0000beef"],
            }
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {unresolved:?}: expected the placement to be refused, got {status} with {parsed}"
    );
    let after_refusal: BTreeSet<String> = interfaces_held_by_ec2(&ec2, unresolved)
        .await
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();
    assert_eq!(
        after_refusal, before,
        "case {unresolved:?}: expected a refused create to place no interface anywhere"
    );

    let unplaced = "a Simple AD created with no VpcSettings at all";
    let without_placement = create_directory(
        router,
        json!({ "Name": "noplacedeni.example.com", "Password": PASSWORD, "Size": "Small" }),
        unplaced,
    )
    .await;

    let interfaces = directory_interfaces(&ec2, &without_placement, unplaced).await;
    assert!(
        interfaces.is_empty(),
        "case {unplaced:?}: expected no interface for a directory created without a placement, \
         got {interfaces:?}"
    );
    delete(router, &without_placement, unplaced).await;
    let after: BTreeSet<String> = interfaces_held_by_ec2(&ec2, unplaced)
        .await
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();
    assert_eq!(
        after, before,
        "case {unplaced:?}: expected a teardown with no interface to release to leave the \
         account's interfaces alone"
    );
}
