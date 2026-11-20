// SPDX-License-Identifier: CC0-1.0

//! The security group a directory's servers sit behind, driven end to end over HTTP with the
//! owner of security groups mounted beside this service.
//!
//! RULE: a directory this service places in a VPC is provisioned a security group for the
//! servers it runs there, in the VPC the chosen subnets belong to, and reports that group in
//! the placement it describes — `DirectoryVpcSettingsDescription$SecurityGroupId` ("the
//! domain controller security group identifier for the directory") for a directory whose
//! servers this service runs, `DirectoryConnectSettingsDescription$SecurityGroupId` ("the
//! security group identifier for the AD Connector directory") for an AD Connector — while a
//! create whose placement the owner of subnets holds nothing for is refused and is given no
//! group at all. The group belongs to the placement, so EVERY reporting read of the directory
//! names it — the read that names the directory and the one that lists them, at whatever stage
//! the build has reached — and the owner of security groups resolves that one identifier to
//! exactly one group.
//!
//! The rows differ along what the rule generalises over: the directory type and therefore the
//! placement member the group is reported in (Managed Microsoft AD, Simple AD, AD Connector),
//! the subnets chosen and the blocks they carry, the order the create names them in, whether
//! they are a client's own or the account's default ones, and whether the account has them at
//! all.
//!
//! A security group is an Amazon EC2 resource, so EC2 is mounted beside this service on ONE
//! engine (`crate::directory_dns_addresses::network`, the account network this crate's
//! placement suites share) and every id this suite asserts on is READ BACK from EC2 itself:
//! an id this service invented would name no group there and could not pass. Nothing reaches
//! into either store.
//!
//! One row places a directory in the account's DEFAULT subnets while its settings block names
//! the client's own VPC — a VPC the account really holds, so the create is not refused for it
//! — so the group can only be found where the SUBNETS are, not where the request said.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::ad_connector::connect_directory;
use crate::directory_dns_addresses::{ec2_client, network, AccountSubnet, ABSENT_SUBNET};
// The account's own security-group listing, read through the sibling suite's one reader of it.
use crate::directory_network_release::groups_held_by_ec2;
use crate::directory_lifecycle::{
    create_directory, describe_all, describe_one, ok, string, PASSWORD, STAGE_ACTIVE,
};
use crate::microsoft_ad::create_microsoft_ad;

/// How long a polled read may take to report the directory in service before a case fails:
/// far longer than any span this service publishes, and well inside what a client's waiter
/// allows, so a loaded or faster host changes nothing.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// How often such a read asks again.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// The `CustomerDnsIps` an AD Connector forwards to: addresses of a self-managed network this
/// emulator does not hold, which is what a connector's create names.
const CUSTOMER_DNS_IPS: [&str; 2] = ["10.9.0.10", "10.9.1.10"];

/// Assert `id` is a `SecurityGroupId` the model admits: the member's own pattern,
/// `^(sg-[0-9a-f]{8}|sg-[0-9a-f]{17})$`
/// (`smithy_query(query="shape", shape="SecurityGroupId")`).
fn assert_security_group_id(id: &str, case: &str) {
    let suffix = id.strip_prefix("sg-").unwrap_or_else(|| {
        panic!("case {case:?}: expected a SecurityGroupId starting with sg-, got {id:?}")
    });
    assert!(
        (suffix.len() == 8 || suffix.len() == 17)
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "case {case:?}: expected a SecurityGroupId matching \
         ^(sg-[0-9a-f]{{8}}|sg-[0-9a-f]{{17}})$, got {id:?}"
    );
}

/// The VPC EC2 reports for `subnet_id` — the network the directory's servers really sit on,
/// which is what the group it is given has to be in.
///
/// `pub(crate)` because the sibling suite `directory_server_interfaces` asks the owner of
/// subnets the same question about the interfaces placed there, rather than reading it a
/// second time.
pub(crate) async fn vpc_of(ec2: &::aws_sdk_ec2::Client, subnet_id: &str) -> String {
    let described = ec2
        .describe_subnets()
        .subnet_ids(subnet_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("EC2 must report the subnet {subnet_id}: {error:?}"));
    described
        .subnets()
        .iter()
        .find(|subnet| subnet.subnet_id() == Some(subnet_id))
        .and_then(::aws_sdk_ec2::types::Subnet::vpc_id)
        .unwrap_or_else(|| panic!("EC2 must report a VPC for the subnet {subnet_id}"))
        .to_string()
}

/// The ONE group `group_id` names in EC2 — the service that owns security groups — failing the
/// case when it answers any other number.
///
/// An id this service reported that EC2 does not have is exactly what must not pass, and a
/// client that reads the group off the directory then describes that single identifier: it has
/// to resolve there to exactly one group, since an identifier the owner answers nothing for,
/// or more than one group for, is not one a rule can be authorized against.
async fn group_reported_by_ec2(
    ec2: &::aws_sdk_ec2::Client,
    group_id: &str,
    case: &str,
) -> ::aws_sdk_ec2::types::SecurityGroup {
    let described = ec2
        .describe_security_groups()
        .group_ids(group_id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must hold the group {group_id}: {error:?}")
        });
    let groups = described.security_groups();
    assert_eq!(
        groups.len(),
        1,
        "case {case:?}: expected EC2 to resolve {group_id} to exactly one group, got {groups:?}"
    );
    let group = groups[0].clone();
    assert_eq!(
        group.group_id(),
        Some(group_id),
        "case {case:?}: expected EC2 to report the group {group_id}, got {group:?}"
    );
    group
}

/// The directory `directory_id`, polled through `DescribeDirectories` until it reports itself
/// in service, so a case can assert on a directory whose build has finished.
async fn described_in_service(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let described = describe_one(router, directory_id, case).await;
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

/// The identifiers of `subnets`, in the order a create names them.
fn subnet_ids(subnets: &[AccountSubnet]) -> Vec<String> {
    subnets.iter().map(|subnet| subnet.id.clone()).collect()
}

/// A `CreateMicrosoftAD` body placing a directory in `subnets`, with `vpc_id` as the `VpcId`
/// its settings block names.
fn microsoft_ad_in(name: &str, vpc_id: &str, subnets: &[AccountSubnet]) -> Value {
    json!({
        "Name": name,
        "Password": PASSWORD,
        "VpcSettings": { "VpcId": vpc_id, "SubnetIds": subnet_ids(subnets) }
    })
}

/// A `CreateDirectory` (Simple AD) body placing a directory in `subnets`.
fn simple_ad_in(name: &str, vpc_id: &str, subnets: &[AccountSubnet]) -> Value {
    json!({
        "Name": name,
        "Password": PASSWORD,
        "Size": "Small",
        "VpcSettings": { "VpcId": vpc_id, "SubnetIds": subnet_ids(subnets) }
    })
}

/// A `ConnectDirectory` body placing an AD Connector's servers in `subnets`.
fn connector_in(name: &str, vpc_id: &str, subnets: &[AccountSubnet]) -> Value {
    json!({
        "Name": name,
        "Password": PASSWORD,
        "Size": "Small",
        "ConnectSettings": {
            "VpcId": vpc_id,
            "SubnetIds": subnet_ids(subnets),
            "CustomerDnsIps": CUSTOMER_DNS_IPS,
            "CustomerUserName": "ad-connector-svc",
        }
    })
}

/// Which create a row drives, and so which placement member the directory reports its group
/// in: the modelled `DirectoryDescription` carries both, and a directory's type decides which
/// one it fills.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `CreateMicrosoftAD`, reported as `VpcSettings`.
    MicrosoftAd,
    /// `CreateDirectory` (Simple AD), reported as `VpcSettings`.
    SimpleAd,
    /// `ConnectDirectory` (AD Connector), reported as `ConnectSettings`.
    Connector,
}

impl Kind {
    /// The member of the description this kind of directory reports its placement — and so
    /// its security group — in.
    fn placement(self) -> &'static str {
        match self {
            Kind::MicrosoftAd | Kind::SimpleAd => "VpcSettings",
            Kind::Connector => "ConnectSettings",
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
    match kind {
        Kind::MicrosoftAd => {
            create_microsoft_ad(router, microsoft_ad_in(name, vpc_id, subnets), case).await
        }
        Kind::SimpleAd => {
            create_directory(router, simple_ad_in(name, vpc_id, subnets), case).await
        }
        Kind::Connector => {
            connect_directory(router, connector_in(name, vpc_id, subnets), case).await
        }
    }
}

/// RULE: every directory this service places in a VPC reports a security group for its
/// servers, in the placement member its own type describes, and that group is one the owner of
/// security groups really holds — in the VPC the chosen subnets belong to, named after the
/// directory it belongs to.
#[tokio::test]
async fn a_directory_placed_in_a_vpc_reports_the_security_group_ec2_holds_for_its_servers() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));

    // The rows differ in the directory type (and so in the placement member the group is
    // reported in), in the blocks of the chosen subnets, in the order the create names them,
    // and in whether the subnets are the client's own or the account's default ones.
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
            "a Simple AD in two /24 subnets",
            Kind::SimpleAd,
            vec![network.client[1].clone(), network.client[0].clone()],
        ),
        (
            "an AD Connector in a /20 and a /28",
            Kind::Connector,
            vec![network.client[3].clone(), network.client[2].clone()],
        ),
    ];

    let mut group_ids: BTreeSet<String> = BTreeSet::new();
    for (index, (case, kind, subnets)) in rows.iter().enumerate() {
        let name = format!("sg{index}.example.com");
        let placement = kind.placement();
        // Every row names the VPC its own chosen subnets sit in, as EC2 reports it: a
        // directory's servers are placed in subnets OF the VPC the create names, so a row
        // placed in the account's default subnets names the default VPC and one placed in
        // the client's own names the VPC `ec2:CreateVpc` minted.
        let directory_id = create_in(router, *kind, &name, &subnets[0].vpc, subnets, case).await;

        // The group is part of the network the create provisions, so it is reported from the
        // create on, before the build the directory owes has finished.
        let created = describe_one(router, &directory_id, case).await;
        let at_create = string(&created[placement], "SecurityGroupId", case);
        assert_security_group_id(&at_create, case);

        // ...and the same group is still reported once the directory is in service: a read
        // provisions nothing and changes nothing.
        let described = described_in_service(router, &directory_id, case).await;
        let reported = string(&described[placement], "SecurityGroupId", case);
        assert_eq!(
            reported, at_create,
            "case {case:?}: expected the group reported at create to be the one reported in \
             service, got {described}"
        );

        // The group EC2 HOLDS: in the VPC the chosen subnets belong to, as EC2 reports both,
        // and named after the directory whose servers it is for — Directory Service names the
        // group it creates for a directory `<DirectoryId>_controllers`.
        let group = group_reported_by_ec2(&ec2, &reported, case).await;
        let expected_vpc = vpc_of(&ec2, &subnets[0].id).await;
        assert_eq!(
            group.vpc_id(),
            Some(expected_vpc.as_str()),
            "case {case:?}: expected the group in the VPC of the chosen subnets \
             ({expected_vpc}), got {group:?}"
        );
        assert_eq!(
            group.group_name(),
            Some(format!("{directory_id}_controllers").as_str()),
            "case {case:?}: expected the group named after {directory_id}, got {group:?}"
        );
        assert!(
            group
                .description()
                .is_some_and(|description| description.contains(&directory_id)),
            "case {case:?}: expected the group's description to name {directory_id}, got \
             {group:?}"
        );

        // The directory reports the VPC its create NAMED, which is what this service echoes:
        // only the group is resolved against the network the servers are really on.
        assert_eq!(
            described[placement]["VpcId"],
            json!(&subnets[0].vpc),
            "case {case:?}: expected the requested VpcId to be echoed, got {described}"
        );

        // No two directories share a group: each one is given its own.
        assert!(
            group_ids.insert(reported.clone()),
            "case {case:?}: expected a group of this directory's own, got {reported} again"
        );
    }
}

/// RULE: the group exists for the directory's servers, so it does not outlive the
/// directory: a `DeleteDirectory` releases it in the service that owns security groups, so a
/// client reconciling the account against Amazon EC2 after the teardown finds no group of a
/// directory that is gone — and the groups of every OTHER directory are untouched.
///
/// The rows differ along what the rule generalises over: the directory type, and so which
/// create provisioned the group and which placement member carries it.
#[tokio::test]
async fn deleting_a_directory_releases_the_security_group_ec2_held_for_its_servers() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));

    let rows: [(&str, Kind, Vec<AccountSubnet>); 3] = [
        (
            "a Managed Microsoft AD",
            Kind::MicrosoftAd,
            vec![network.client[0].clone(), network.client[1].clone()],
        ),
        (
            "a Simple AD",
            Kind::SimpleAd,
            vec![network.client[2].clone(), network.client[3].clone()],
        ),
        (
            "an AD Connector",
            Kind::Connector,
            vec![network.client[1].clone(), network.client[0].clone()],
        ),
    ];

    // Every row's directory and group, created before any of them is deleted, so a delete
    // can be shown to release its OWN group and no other.
    let mut held: Vec<(&str, String, String)> = Vec::new();
    for (index, (case, kind, subnets)) in rows.iter().enumerate() {
        let name = format!("sgdelete{index}.example.com");
        let directory_id = create_in(router, *kind, &name, &network.vpc, subnets, case).await;
        let described = described_in_service(router, &directory_id, case).await;
        let group_id = string(&described[kind.placement()], "SecurityGroupId", case);
        group_reported_by_ec2(&ec2, &group_id, case).await;
        held.push((case, directory_id, group_id));
    }

    for (position, (case, directory_id, group_id)) in held.iter().enumerate() {
        ok(
            router,
            "DeleteDirectory",
            json!({ "DirectoryId": directory_id }),
            case,
        )
        .await;

        // The group is gone from the owner of security groups: polled, because a teardown is
        // work the service carries out rather than something a caller waits on inline.
        let started = std::time::Instant::now();
        loop {
            let described = ec2
                .describe_security_groups()
                .group_ids(group_id)
                .send()
                .await;
            let still_held = described.is_ok_and(|answer| {
                answer
                    .security_groups()
                    .iter()
                    .any(|group| group.group_id() == Some(group_id.as_str()))
            });
            if !still_held {
                break;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: expected the group {group_id} of the deleted {directory_id} to \
                 be released within {POLL_DEADLINE:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // A repeated delete releases nothing twice and is still served.
        ok(
            router,
            "DeleteDirectory",
            json!({ "DirectoryId": directory_id }),
            case,
        )
        .await;

        // And the groups of the directories this delete did not name are untouched.
        for (other_case, other_directory, other_group) in held.iter().skip(position + 1) {
            let group = group_reported_by_ec2(&ec2, other_group, other_case).await;
            assert_eq!(
                group.group_id(),
                Some(other_group.as_str()),
                "case {case:?}: expected the group of {other_directory} to survive the delete \
                 of {directory_id}, got {group:?}"
            );
        }
    }
}

/// RULE: nothing is provisioned for a placement the account does not have — a create naming an
/// identifier the owner of subnets reports nothing for is refused before any network is asked
/// for, so no group is minted anywhere and the account's own groups are left exactly as they
/// were.
///
/// The rows differ in how little of the placement the account holds: neither identifier, in
/// either order.
#[tokio::test]
async fn a_create_whose_placement_does_not_resolve_is_given_no_security_group() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let before = groups_held_by_ec2(&ec2, "before any refused create").await;

    // The rows differ in how little of the placement resolves: none of it, and a pair the
    // modelled pattern admits that EC2 mints no subnet for either way.
    let rows: [(&str, Vec<String>); 2] = [
        (
            "two identifiers the account has no subnet for",
            vec![ABSENT_SUBNET.to_string(), "subnet-0000beef".to_string()],
        ),
        (
            "an absent pair in the other order",
            vec!["subnet-0000beef".to_string(), ABSENT_SUBNET.to_string()],
        ),
    ];

    for (index, (case, ids)) in rows.iter().enumerate() {
        // The VPC is one the account really holds, so the only thing the owner of subnets
        // reports nothing for is the subnets.
        let body = json!({
            "Name": format!("nosg{index}.example.com"),
            "Password": PASSWORD,
            "VpcSettings": { "VpcId": network.vpc, "SubnetIds": ids }
        });
        let (status, parsed) =
            crate::directory_lifecycle::call(router, "CreateMicrosoftAD", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a placement the account has no subnet for to be refused, \
             got {status} with {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!("ClientException"),
            "case {case:?}: expected the resource-semantics refusal, got {parsed}"
        );
        assert_eq!(
            groups_held_by_ec2(&ec2, case).await,
            before,
            "case {case:?}: expected a refused create to ask the owner of security groups for \
             nothing"
        );
    }
}

/// RULE: a directory created with no VPC settings at all — the AD Connector's sibling, a
/// Simple AD created without a placement — has no network to be given a group in and reports
/// neither placement member.
#[tokio::test]
async fn a_directory_created_without_a_placement_reports_no_security_group() {
    let network = network().await;
    let router = &network.router;
    let case = "a Simple AD created with no VpcSettings";

    let directory_id = create_directory(
        router,
        json!({ "Name": "noplacement.example.com", "Password": PASSWORD, "Size": "Small" }),
        case,
    )
    .await;

    let described = describe_one(router, &directory_id, case).await;
    assert!(
        described["VpcSettings"].is_null() && described["ConnectSettings"].is_null(),
        "case {case:?}: expected no placement at all for a directory created without one, got \
         {described}"
    );
}

/// The description the UNFILTERED `DescribeDirectories` page carries for `directory_id` — the
/// read a client listing its directories makes, as against the one that names the id.
async fn described_in_the_page(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Value {
    describe_all(router, case)
        .await
        .into_iter()
        .find(|described| described["DirectoryId"] == json!(directory_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the unfiltered page to carry {directory_id}")
        })
}

/// RULE: the domain controller security group is part of the placement a directory reports, so
/// EVERY reporting read of a VPC-placed directory names it — the read that names the directory
/// and the one that lists them, at whatever stage the build has reached — always the same
/// identifier, of the shape the modelled member admits
/// (`^(sg-[0-9a-f]{8}|sg-[0-9a-f]{17})$`), and the owner of security groups resolves that one
/// identifier to exactly one group, in the VPC the directory's servers really sit in.
///
/// This is what a client polling for the directory's network footprint depends on: a reader
/// that reported the group only once the build had settled, or only through one of the two
/// reads, would leave the poll waiting on a member that never arrives.
///
/// The rows differ along what the rule generalises over: the directory type and so the
/// placement member the group is reported in, the blocks of the chosen subnets, the order the
/// create names them in, and whether they are a client's own subnets or the account's default
/// ones.
#[tokio::test]
async fn every_reporting_read_of_a_placed_directory_names_the_one_group_ec2_holds_for_it() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));

    let rows: [(&str, Kind, Vec<AccountSubnet>); 4] = [
        (
            "a Managed Microsoft AD in two /24 subnets",
            Kind::MicrosoftAd,
            vec![network.client[0].clone(), network.client[1].clone()],
        ),
        (
            "a Managed Microsoft AD in a /20 and a /28, named in that order",
            Kind::MicrosoftAd,
            vec![network.client[3].clone(), network.client[2].clone()],
        ),
        (
            "a Simple AD in the account's default subnets",
            Kind::SimpleAd,
            vec![network.default[0].clone(), network.default[1].clone()],
        ),
        (
            "an AD Connector in a /28 and a /24",
            Kind::Connector,
            vec![network.client[2].clone(), network.client[0].clone()],
        ),
    ];

    let mut reported: BTreeSet<String> = BTreeSet::new();
    for (index, (case, kind, subnets)) in rows.iter().enumerate() {
        let name = format!("polled{index}.example.com");
        let placement = kind.placement();
        let directory_id = create_in(router, *kind, &name, &subnets[0].vpc, subnets, case).await;

        // Poll the way a client waiting for the directory's network does, and hold every read
        // to the rule: the group is named, is of the modelled shape, and is the one the last
        // read named — whichever stage the build is at, and through either reporting read.
        let started = std::time::Instant::now();
        let mut first: Option<String> = None;
        let mut stages: BTreeSet<String> = BTreeSet::new();
        loop {
            let described = describe_one(router, &directory_id, case).await;
            let listed = described_in_the_page(router, &directory_id, case).await;
            let stage = string(&described, "Stage", case);
            let group_id = string(&described[placement], "SecurityGroupId", case);
            assert_security_group_id(&group_id, case);
            assert_eq!(
                string(&listed[placement], "SecurityGroupId", case),
                group_id,
                "case {case:?}: expected the listing to name the group the targeted read does, \
                 got {listed}"
            );
            let expected = first.clone().unwrap_or_else(|| group_id.clone());
            assert_eq!(
                group_id, expected,
                "case {case:?}: expected every read to name one group, got {group_id} at stage \
                 {stage:?} after {expected}"
            );
            first = Some(group_id);
            stages.insert(stage.clone());
            if stage == STAGE_ACTIVE {
                break;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: expected {directory_id} to report itself in service within \
                 {POLL_DEADLINE:?}, stages observed {stages:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let group_id =
            first.unwrap_or_else(|| panic!("case {case:?}: the poll above reads at least once"));

        // That one identifier resolves, in the owner of security groups, to exactly one group —
        // in the VPC the chosen subnets belong to, which is the network the servers are on.
        let group = group_reported_by_ec2(&ec2, &group_id, case).await;
        let expected_vpc = vpc_of(&ec2, &subnets[0].id).await;
        assert_eq!(
            group.vpc_id(),
            Some(expected_vpc.as_str()),
            "case {case:?}: expected the group in the VPC of the chosen subnets \
             ({expected_vpc}), got {group:?}"
        );

        // And it is this directory's own: no two of the rows were given one group.
        assert!(
            reported.insert(group_id.clone()),
            "case {case:?}: expected a group of this directory's own, got {group_id} again"
        );
    }
}
