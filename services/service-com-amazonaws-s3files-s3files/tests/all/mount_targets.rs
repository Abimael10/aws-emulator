// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the S3 Files mount-target slice: `CreateMountTarget`,
//! `GetMountTarget`, `ListMountTargets`, `UpdateMountTarget` and `DeleteMountTarget`,
//! plus what a mount target means for `DeleteFileSystem`.
//!
//! Every case drives the service the way a client does — one restJson1 HTTP request per
//! call, through the same `MultiServiceRouter` the emulator boots — so what is asserted is
//! the emulator's HTTP behaviour, not a handler called directly. Amazon EC2 is mounted
//! beside it on the SAME engine, because a mount target is placed in a subnet and a subnet
//! is EC2's resource: every subnet, VPC, zone and security group a case names is one EC2
//! minted, and every fact a response reports about the placement is compared against what
//! EC2 itself answers for that subnet, never against a value written out here.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. A create places the mount target in the subnet the request names and reports that
//!    subnet's VPC, Availability Zone and an address out of its CIDR block; a read answers
//!    from what was persisted, member for member.
//! 2. An address is the one the request named when it named one — refused when it is
//!    outside the subnet's block or already taken — and otherwise one the subnet has free.
//! 3. A file system holds at most one mount target per Availability Zone: a second create
//!    in a zone it already has one in is refused, whichever subnet of that zone it names,
//!    while another file system may have its own mount target in the same zone.
//! 4. A delete makes the mount target observably gone and releases its zone, so the file
//!    system can be given a mount target there again.
//! 5. A listing answers every persisted mount target matching its filter, in one stable
//!    order, cut into pages of at most `maxResults` with a `nextToken` that resumes exactly
//!    where the previous page stopped.
//! 6. An update REPLACES the security groups, and the replacement is what every later read
//!    reports.
//! 7. A resource reported `creating` becomes `available` because the clock made the
//!    transition due, not because anybody read it.
//! 8. A file system with a mount target is not deleted unless the caller asked for its
//!    children to go with it.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::file_system_lifecycle::{
    call, created_file_system_id, encoded, is_minted_id, Answer, ACCOUNT, REGION,
};

/// A generous deadline for a transition the clock owes to have come due. Nothing waits for
/// it: [`settled_status`] looks again until the condition holds or this passes, so a loaded
/// or a fast machine both reach the same verdict.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long [`settled_status`] waits between two looks. Inside the poll loop, so it bounds
/// how often the condition is re-read, never how long the test may take.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// An IPv4 block of the account's default VPC (`172.31.0.0/16`) that no default subnet
/// takes: those are the first `/20` blocks of the VPC, one per Availability Zone, so the
/// last `/20` is free for the extra subnet rule 3 needs in a zone that already has one.
const SPARE_SUBNET_BLOCK: &str = "172.31.240.0/20";

/// A CIDR block for a SECOND VPC, well away from the default VPC's, so a security group
/// created in it is provably a group of another VPC.
const OTHER_VPC_BLOCK: &str = "10.9.0.0/16";

/// One subnet of the account, exactly as EC2 reports it. Everything a mount target reports
/// about its placement is compared against these values.
#[derive(Debug, Clone)]
struct SubnetFacts {
    subnet_id: String,
    vpc_id: String,
    zone_name: String,
    availability_zone_id: String,
    cidr_block: String,
}

impl SubnetFacts {
    /// The `offset`-th address of this subnet's block, for a case that names an address
    /// explicitly. Read off the block EC2 reported, so the address is one of the subnet's
    /// own whatever block the account's network has.
    fn address_at(&self, offset: u32) -> String {
        let block: ::ipnet::Ipv4Net = self
            .cidr_block
            .parse()
            .unwrap_or_else(|error| panic!("EC2's block {:?} must parse: {error}", self.cidr_block));
        Ipv4Addr::from(u32::from(block.network()).saturating_add(offset)).to_string()
    }

    /// Whether `address` lies inside this subnet's block.
    fn holds(&self, address: &str) -> bool {
        match (
            self.cidr_block.parse::<::ipnet::Ipv4Net>(),
            address.parse::<Ipv4Addr>(),
        ) {
            (Ok(block), Ok(address)) => block.contains(&address),
            _ => false,
        }
    }
}

/// One emulator over one shared engine: this service the way the router mounts it in
/// production, PLUS Amazon EC2 — the service that owns subnets — because every mount
/// target is placed in one. An EC2 client over the router's own in-process callback comes
/// with it: the very seam the service resolves a subnet through, and what a case builds its
/// extra network with.
struct Network {
    router: MultiServiceRouter,
    ec2: ::aws_sdk_ec2::Client,
    subnets: Vec<SubnetFacts>,
}

/// An EC2 client for [`REGION`] over `callback`, the in-process seam this emulator routes
/// on: no socket is opened and the endpoint URL is only the address that transport routes
/// by.
fn ec2_client(callback: &EmbeddedEmulator) -> ::aws_sdk_ec2::Client {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
        .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// The subnets the account has in [`REGION`], as EC2 answers for them, ordered by
/// Availability Zone: the default VPC's default subnets, whose ids EC2 minted. Nothing here
/// derives an identifier or a zone of its own.
async fn account_subnets(ec2: &::aws_sdk_ec2::Client) -> Vec<SubnetFacts> {
    let described = ec2
        .describe_subnets()
        .send()
        .await
        .expect("EC2 must report the account's subnets");
    let mut facts: Vec<SubnetFacts> = described
        .subnets()
        .iter()
        .filter_map(|subnet| {
            Some(SubnetFacts {
                subnet_id: subnet.subnet_id()?.to_string(),
                vpc_id: subnet.vpc_id()?.to_string(),
                zone_name: subnet.availability_zone()?.to_string(),
                availability_zone_id: subnet.availability_zone_id()?.to_string(),
                cidr_block: subnet.cidr_block()?.to_string(),
            })
        })
        .filter(|facts| facts.zone_name.starts_with(REGION))
        .collect();
    facts.sort_by(|left, right| left.availability_zone_id.cmp(&right.availability_zone_id));
    facts
}

async fn network() -> Network {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3files_s3files::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let subnets = account_subnets(&ec2).await;
    assert!(
        subnets.len() >= 3,
        "the account must have at least three subnets in {REGION} for these cases, got \
         {subnets:?}"
    );
    Network {
        router,
        ec2,
        subnets,
    }
}

impl Network {
    /// The default security group of `vpc_id`, as EC2 reports it — what a mount target
    /// created without `securityGroups` must report, ASKED rather than assumed.
    async fn default_security_groups(&self, vpc_id: &str) -> Vec<String> {
        let described = self
            .ec2
            .describe_security_groups()
            .filters(
                ::aws_sdk_ec2::types::Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id)
                    .build(),
            )
            .filters(
                ::aws_sdk_ec2::types::Filter::builder()
                    .name("group-name")
                    .values("default")
                    .build(),
            )
            .send()
            .await
            .expect("EC2 must report the VPC's security groups");
        described
            .security_groups()
            .iter()
            .filter_map(|group| group.group_id().map(str::to_string))
            .collect()
    }

    /// A security group EC2 minted in `vpc_id`, so a case names a group the account really
    /// has.
    async fn security_group(&self, vpc_id: &str, name: &str) -> String {
        self.ec2
            .create_security_group()
            .group_name(name)
            .description("s3files mount-target suite")
            .vpc_id(vpc_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("CreateSecurityGroup({name}) must succeed: {error:?}"))
            .group_id()
            .unwrap_or_else(|| panic!("CreateSecurityGroup({name}) must answer a group id"))
            .to_string()
    }

    /// A second subnet EC2 minted in the SAME Availability Zone as `zone_name`, so rule 3
    /// is exercised over two DIFFERENT subnets of one zone.
    async fn extra_subnet_in_zone(&self, vpc_id: &str, zone_name: &str) -> String {
        self.ec2
            .create_subnet()
            .vpc_id(vpc_id)
            .cidr_block(SPARE_SUBNET_BLOCK)
            .availability_zone(zone_name)
            .send()
            .await
            .unwrap_or_else(|error| panic!("CreateSubnet in {zone_name} must succeed: {error:?}"))
            .subnet()
            .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
            .unwrap_or_else(|| panic!("CreateSubnet in {zone_name} must answer a subnet id"))
            .to_string()
    }

    /// The network interface EC2 — the service that OWNS network interfaces — holds under
    /// `network_interface_id`, or `None` when it holds none.
    ///
    /// Asked by filter rather than by id, so an id the account has no interface for is an
    /// absent row instead of a failure: that is what lets a case assert BOTH that an
    /// interface was really created and that it was really released.
    async fn network_interface(
        &self,
        network_interface_id: &str,
    ) -> Option<::aws_sdk_ec2::types::NetworkInterface> {
        self.ec2
            .describe_network_interfaces()
            .filters(
                ::aws_sdk_ec2::types::Filter::builder()
                    .name("network-interface-id")
                    .values(network_interface_id)
                    .build(),
            )
            .send()
            .await
            .expect("EC2 must report the account's network interfaces")
            .network_interfaces()
            .iter()
            .find(|interface| {
                interface.network_interface_id() == Some(network_interface_id)
            })
            .cloned()
    }

    /// A VPC EC2 minted that is NOT the one the account's subnets belong to.
    async fn other_vpc(&self) -> String {
        self.ec2
            .create_vpc()
            .cidr_block(OTHER_VPC_BLOCK)
            .send()
            .await
            .unwrap_or_else(|error| panic!("CreateVpc must succeed: {error:?}"))
            .vpc()
            .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
            .unwrap_or_else(|| panic!("CreateVpc must answer a VPC id"))
            .to_string()
    }
}

/// `CreateMountTarget` with `request` as its body.
async fn create_mount_target(router: &MultiServiceRouter, request: Value) -> Answer {
    call(router, "PUT", "/mount-targets", Some(request)).await
}

/// `CreateMountTarget` for `file_system_id` in `subnet_id`, asserting it was accepted, and
/// returning the whole answer.
async fn created_mount_target(
    router: &MultiServiceRouter,
    file_system_id: &str,
    subnet_id: &str,
) -> Answer {
    let created = create_mount_target(
        router,
        json!({ "fileSystemId": file_system_id, "subnetId": subnet_id }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreateMountTarget({file_system_id}, {subnet_id}): expected 200, got {} / {:?}",
        created.status, created.body
    );
    created
}

/// `GetMountTarget`.
async fn get_mount_target(router: &MultiServiceRouter, mount_target_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/mount-targets/{mount_target_id}"),
        None,
    )
    .await
}

/// `ListMountTargets` with `query` appended to its path.
async fn list_mount_targets(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, "GET", &format!("/mount-targets{query}"), None).await
}

/// The `mountTargetId`s a `ListMountTargets` answer reports, in the order it reported
/// them.
fn listed_ids(listed: &Answer) -> Vec<String> {
    listed.field("mountTargets")
        .as_array()
        .unwrap_or_else(|| panic!("expected a mountTargets list in {:?}", listed.body))
        .iter()
        .map(|row| row["mountTargetId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The string list a response member carries.
fn text_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The status `mount_target_id` settles on: looked for again until it is no longer
/// `creating` or [`SETTLE_DEADLINE`] passes. Nothing sleeps for the transition — the
/// condition is what is waited on.
async fn settled_status(router: &MultiServiceRouter, mount_target_id: &str) -> String {
    let started = Instant::now();
    loop {
        let read = get_mount_target(router, mount_target_id).await;
        let status = read.text("status");
        if status != "creating" || started.elapsed() > SETTLE_DEADLINE {
            return status;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of rule 1 and 2: which subnet the mount target goes in, what the request says
/// about its address type and address, and which security groups it names.
struct CreateRow {
    case: &'static str,
    /// Which of the account's subnets, by index into [`Network::subnets`].
    subnet: usize,
    /// `ipAddressType`, when the row names one.
    ip_address_type: Option<&'static str>,
    /// `ipv4Address`, as an offset into the subnet's own block, when the row names one.
    ipv4_offset: Option<u32>,
    /// Whether the row names its own `securityGroups` (one EC2 minted) or leaves the
    /// default group of the subnet's VPC to be resolved.
    names_security_groups: bool,
}

/// Rules 1, 2 and 7: a create places the mount target in the subnet it names, reporting
/// that subnet's own facts, and a read answers from the stored record.
#[tokio::test]
async fn a_create_places_the_mount_target_in_the_subnet_and_reads_back() {
    let network = network().await;
    let router = &network.router;
    let first = created_file_system_id(router, "arn:aws:s3:::s3files-mount-first", Some("mt-1"))
        .await;
    let second = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-second",
        Some("mt-2"),
    )
    .await;
    let group = network
        .security_group(&network.subnets[2].vpc_id, "s3files-mt-named")
        .await;

    // The rows differ along what the rule generalises over: the subnet (and so the zone,
    // the VPC and the block), whether the request names an address type, whether it names
    // an address, and whether it names its own security groups. The last row is a SECOND
    // file system in a zone the first already uses, which the per-file-system rule allows.
    let rows = [
        CreateRow {
            case: "every placement fact resolved",
            subnet: 0,
            ip_address_type: None,
            ipv4_offset: None,
            names_security_groups: false,
        },
        CreateRow {
            case: "IPV4_ONLY with an address the request names",
            subnet: 1,
            ip_address_type: Some("IPV4_ONLY"),
            ipv4_offset: Some(100),
            names_security_groups: false,
        },
        CreateRow {
            case: "security groups the request names",
            subnet: 2,
            ip_address_type: None,
            ipv4_offset: None,
            names_security_groups: true,
        },
        CreateRow {
            case: "another file system in a zone already in use",
            subnet: 0,
            ip_address_type: Some("IPV4_ONLY"),
            ipv4_offset: Some(200),
            names_security_groups: false,
        },
    ];

    let mut created_ids = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let subnet = &network.subnets[row.subnet];
        let file_system_id = if index == 3 { &second } else { &first };
        let mut request = json!({
            "fileSystemId": file_system_id,
            "subnetId": subnet.subnet_id,
        });
        if let Some(ip_address_type) = row.ip_address_type {
            request["ipAddressType"] = json!(ip_address_type);
        }
        if let Some(offset) = row.ipv4_offset {
            request["ipv4Address"] = json!(subnet.address_at(offset));
        }
        if row.names_security_groups {
            request["securityGroups"] = json!([group]);
        }

        let created = create_mount_target(router, request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected 200, got {} / {:?}",
            row.case, created.status, created.body
        );

        let mount_target_id = created.text("mountTargetId");
        assert!(
            is_minted_id("fsmt-", &mount_target_id),
            "case {:?}: mountTargetId must satisfy ^fsmt-[0-9a-f]{{17,40}}$, got \
             {mount_target_id:?}",
            row.case
        );
        // The interface the mount target answers on is EC2's resource, so the owner of
        // network interfaces must really hold it — in that subnet, at the address the
        // mount target reports, in the groups it reports. An id this service minted itself
        // would resolve to nothing here.
        let interface_id = created.text("networkInterfaceId");
        let interface = network
            .network_interface(&interface_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: EC2 must hold the network interface {interface_id} the \
                     mount target reports",
                    row.case
                )
            });
        assert_eq!(
            interface.subnet_id(),
            Some(subnet.subnet_id.as_str()),
            "case {:?}: the interface must sit in the subnet the mount target is in, got \
             {:?}",
            row.case,
            interface.subnet_id()
        );
        assert_eq!(
            interface.private_ip_address(),
            Some(created.text("ipv4Address").as_str()),
            "case {:?}: the address the mount target reports must be the one its interface \
             holds, got {:?}",
            row.case,
            interface.private_ip_address()
        );
        assert_eq!(
            interface
                .groups()
                .iter()
                .filter_map(|group| group.group_id().map(str::to_string))
                .collect::<Vec<String>>(),
            text_list(created.field("securityGroups")),
            "case {:?}: the groups the mount target reports must be the ones its interface \
             sits in, got {:?}",
            row.case,
            interface.groups()
        );
        assert_eq!(
            created.text("fileSystemId"),
            *file_system_id,
            "case {:?}: the mount target must name the file system it is an endpoint for, \
             got {:?}",
            row.case,
            created.body
        );
        assert_eq!(
            created.text("ownerId"),
            ACCOUNT,
            "case {:?}: ownerId must be this emulator's account, got {:?}",
            row.case,
            created.body
        );
        // The placement facts: each compared against what EC2 — the owner of the subnet —
        // answers for it, so a value built here instead of resolved there cannot pass.
        assert_eq!(
            created.text("subnetId"),
            subnet.subnet_id,
            "case {:?}: subnetId must be the subnet the request named, got {:?}",
            row.case,
            created.body
        );
        assert_eq!(
            created.text("vpcId"),
            subnet.vpc_id,
            "case {:?}: vpcId must be the VPC EC2 reports for that subnet, got {:?}",
            row.case,
            created.body
        );
        assert_eq!(
            created.text("availabilityZoneId"),
            subnet.availability_zone_id,
            "case {:?}: availabilityZoneId must be the zone EC2 reports for that subnet, \
             got {:?}",
            row.case,
            created.body
        );
        let address = created.text("ipv4Address");
        assert!(
            subnet.holds(&address),
            "case {:?}: the address must come out of the subnet's block {}, got \
             {address:?}",
            row.case,
            subnet.cidr_block
        );
        if let Some(offset) = row.ipv4_offset {
            assert_eq!(
                address,
                subnet.address_at(offset),
                "case {:?}: an address the request named must be the one assigned, got {:?}",
                row.case,
                created.body
            );
        }
        assert_eq!(
            created.field("ipv6Address"),
            &Value::Null,
            "case {:?}: an IPv4-only mount target carries no IPv6 address, got {:?}",
            row.case,
            created.body
        );
        let expected_groups = if row.names_security_groups {
            vec![group.clone()]
        } else {
            network.default_security_groups(&subnet.vpc_id).await
        };
        assert!(
            !expected_groups.is_empty(),
            "case {:?}: the suite must have a security group to compare against",
            row.case
        );
        assert_eq!(
            text_list(created.field("securityGroups")),
            expected_groups,
            "case {:?}: the interface must sit in the groups the rule names, got {:?}",
            row.case,
            created.body
        );

        // Round trip: the read answers from the same stored record, member for member.
        let read = get_mount_target(router, &mount_target_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the mount target must be persisted, got {} / {:?}",
            row.case, read.status, read.body
        );
        assert_eq!(
            crate::file_system_lifecycle::without(&read.body, &["status"]),
            crate::file_system_lifecycle::without(&created.body, &["status"]),
            "case {:?}: the read must report what the create stored, got {:?}",
            row.case,
            read.body
        );

        // Rule 7: the transition is the clock's, and it is owed whether or not anybody
        // reads the record.
        assert_eq!(
            settled_status(router, &mount_target_id).await,
            "available",
            "case {:?}: the transition the clock owed was never materialised",
            row.case
        );
        created_ids.push(mount_target_id);
    }

    // Every create persisted a DISTINCT mount target, each reachable on its own id.
    let distinct: BTreeSet<&String> = created_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        created_ids.len(),
        "each create must mint its own mount target, got {created_ids:?}"
    );
}

/// Rule 3: one mount target per Availability Zone per file system — whichever subnet of
/// that zone the second create names.
#[tokio::test]
async fn a_file_system_holds_at_most_one_mount_target_per_availability_zone() {
    let network = network().await;
    let router = &network.router;
    let file_system_id =
        created_file_system_id(router, "arn:aws:s3:::s3files-mount-zones", Some("mt-zone")).await;
    let subnet = network.subnets[0].clone();
    let held = created_mount_target(router, &file_system_id, &subnet.subnet_id).await;
    let held_id = held.text("mountTargetId");

    // A second subnet of the SAME zone: a different subnet id, the same zone, so the rule
    // still refuses it — which is what makes the rule a zone rule rather than a subnet
    // one.
    let same_zone_subnet = network
        .extra_subnet_in_zone(&subnet.vpc_id, &subnet.zone_name)
        .await;

    for (case, subnet_id) in [
        ("the same subnet again", subnet.subnet_id.clone()),
        ("another subnet of the same zone", same_zone_subnet),
    ] {
        let refused = create_mount_target(
            router,
            json!({ "fileSystemId": &file_system_id, "subnetId": subnet_id }),
        )
        .await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected the modelled 409, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ConflictException",
            "case {case:?}: expected the modelled ConflictException, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.text("resourceType"),
            "MountTarget",
            "case {case:?}: resourceType names the conflicting resource's type, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.text("resourceId"),
            held_id,
            "case {case:?}: resourceId names the mount target that already serves the zone, \
             got {:?}",
            refused.body
        );
        // The condition's own sentence, naming the endpoint that already serves the zone
        // and the zone the way a caller names one.
        assert_eq!(
            refused.text("message"),
            format!(
                "A mount target {held_id} already exists for this file system in {}'s \
                 availability zone.",
                subnet.zone_name
            ),
            "case {case:?}: expected the zone conflict's own sentence, got {:?}",
            refused.body
        );
        assert_eq!(
            listed_ids(&list_mount_targets(router, &format!("?fileSystemId={file_system_id}")).await),
            vec![held_id.clone()],
            "case {case:?}: a refused create must persist nothing"
        );
    }

    // The rule is per FILE SYSTEM: another file system may have its own endpoint in the
    // same zone.
    let other = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-zones-other",
        Some("mt-zone-other"),
    )
    .await;
    let accepted = created_mount_target(router, &other, &subnet.subnet_id).await;
    assert_eq!(
        accepted.text("availabilityZoneId"),
        subnet.availability_zone_id,
        "another file system's mount target sits in the same zone, got {:?}",
        accepted.body
    );
}

/// Rule 4: a delete makes the mount target gone and gives its zone back.
#[tokio::test]
async fn deleting_a_mount_target_frees_its_zone_for_a_new_one() {
    let network = network().await;
    let router = &network.router;
    let file_system_id = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-delete",
        Some("mt-delete"),
    )
    .await;
    let subnet = &network.subnets[0];
    let first = created_mount_target(router, &file_system_id, &subnet.subnet_id).await;
    let first_id = first.text("mountTargetId");
    let first_interface = first.text("networkInterfaceId");
    assert!(
        network.network_interface(&first_interface).await.is_some(),
        "setup: EC2 must hold the interface {first_interface} the mount target reports"
    );

    let deleted = call(
        router,
        "DELETE",
        &format!("/mount-targets/{first_id}"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "DeleteMountTarget must answer the modelled 204, got {} / {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        deleted.body,
        Value::Null,
        "a 204 carries no body, got {:?}",
        deleted.body
    );
    // The interface existed FOR this mount target, so the owner of network interfaces no
    // longer holds it either: nothing is left holding an address of the caller's subnet.
    assert!(
        network.network_interface(&first_interface).await.is_none(),
        "the deleted mount target's interface {first_interface} must be released in EC2"
    );

    // Gone to every later caller, the repeat of the delete included.
    for (case, method) in [("read", "GET"), ("delete again", "DELETE")] {
        let gone = call(router, method, &format!("/mount-targets/{first_id}"), None).await;
        assert_eq!(
            (gone.status, gone.error_type.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a removed mount target must be not found, got {} / {:?}",
            gone.status,
            gone.body
        );
        assert_eq!(
            gone.text("message"),
            "Mount target does not exist.",
            "case {case:?}: expected the condition's own sentence, got {:?}",
            gone.body
        );
    }
    assert!(
        listed_ids(&list_mount_targets(router, &format!("?fileSystemId={file_system_id}")).await)
            .is_empty(),
        "a removed mount target must not be listed"
    );

    // The zone is free again: the identical create now succeeds, with a mount target of
    // its own.
    let again = created_mount_target(router, &file_system_id, &subnet.subnet_id).await;
    assert_ne!(
        again.text("mountTargetId"),
        first_id,
        "the second create must mint a new mount target, got {:?}",
        again.body
    );
    assert_eq!(
        listed_ids(&list_mount_targets(router, &format!("?fileSystemId={file_system_id}")).await),
        vec![again.text("mountTargetId")],
        "the recreated mount target is the one the file system holds"
    );
}

/// Rule 5: the listing answers what is persisted, filtered and paged.
#[tokio::test]
async fn list_mount_targets_filters_by_file_system_and_access_point_and_pages() {
    let network = network().await;
    let router = &network.router;
    let first =
        created_file_system_id(router, "arn:aws:s3:::s3files-mount-list-1", Some("mt-list-1")).await;
    let second =
        created_file_system_id(router, "arn:aws:s3:::s3files-mount-list-2", Some("mt-list-2")).await;

    // Two mount targets under the first file system, in two zones, and one under the
    // second, so a filter that selected the wrong set would be visible.
    let first_ids = vec![
        created_mount_target(router, &first, &network.subnets[0].subnet_id)
            .await
            .text("mountTargetId"),
        created_mount_target(router, &first, &network.subnets[1].subnet_id)
            .await
            .text("mountTargetId"),
    ];
    let second_ids = vec![
        created_mount_target(router, &second, &network.subnets[2].subnet_id)
            .await
            .text("mountTargetId"),
    ];
    let every_id: Vec<String> = first_ids.iter().chain(second_ids.iter()).cloned().collect();

    // An access point selects the mount targets of the file system it belongs to.
    let access_point = crate::file_system_lifecycle::create_access_point(
        router,
        &first,
        json!({ "clientToken": "mt-list-ap" }),
    )
    .await;
    assert_eq!(
        access_point.status, 200,
        "setup CreateAccessPoint: expected 200, got {} / {:?}",
        access_point.status, access_point.body
    );
    let access_point_id = access_point.text("accessPointId");

    for (case, query, expected) in [
        ("no filter", String::new(), every_id.clone()),
        (
            "by the first file system",
            format!("?fileSystemId={first}"),
            first_ids.clone(),
        ),
        (
            "by the second file system",
            format!("?fileSystemId={second}"),
            second_ids.clone(),
        ),
        (
            "by the file system's ARN",
            format!(
                "?fileSystemId={}",
                encoded(&format!(
                    "arn:aws:s3files:{REGION}:{ACCOUNT}:file-system/{first}"
                ))
            ),
            first_ids.clone(),
        ),
        (
            "by an access point of the first file system",
            format!("?accessPointId={access_point_id}"),
            first_ids.clone(),
        ),
        (
            "by an access point and the file system it belongs to",
            format!("?accessPointId={access_point_id}&fileSystemId={first}"),
            first_ids.clone(),
        ),
        (
            "by an access point and a file system it does not belong to",
            format!("?accessPointId={access_point_id}&fileSystemId={second}"),
            Vec::new(),
        ),
    ] {
        let listed = list_mount_targets(router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed_ids(&listed),
            expected,
            "case {case:?}: the listing must answer exactly the persisted set, got {:?}",
            listed.body
        );
        assert_eq!(
            listed.field("nextToken"),
            &Value::Null,
            "case {case:?}: a page that holds everything carries no cursor, got {:?}",
            listed.body
        );
    }

    // Paged one row at a time, the cursor resumes exactly where the page stopped.
    let mut walked = Vec::new();
    let mut query = format!("?fileSystemId={first}&maxResults=1");
    loop {
        let page = list_mount_targets(router, &query).await;
        assert_eq!(
            page.status, 200,
            "case \"paged listing\": expected 200, got {} / {:?}",
            page.status, page.body
        );
        let ids = listed_ids(&page);
        assert!(
            ids.len() <= 1,
            "case \"paged listing\": a page holds at most maxResults rows, got {ids:?}"
        );
        walked.extend(ids);
        match page.field("nextToken").as_str() {
            Some(token) => {
                query = format!("?fileSystemId={first}&maxResults=1&nextToken={}", encoded(token));
            }
            None => break,
        }
        assert!(
            walked.len() <= first_ids.len(),
            "case \"paged listing\": the cursor must not repeat a row, got {walked:?}"
        );
    }
    assert_eq!(
        walked, first_ids,
        "case \"paged listing\": walking the pages must answer the same set, in order"
    );

    for (case, query, status, error) in [
        (
            "an unknown file system",
            "?fileSystemId=fs-0123456789abcdef0".to_string(),
            404,
            "ResourceNotFoundException",
        ),
        (
            "an unknown access point",
            "?accessPointId=fsap-0123456789abcdef0".to_string(),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a cursor this service never issued",
            "?nextToken=not-a-cursor".to_string(),
            400,
            "ValidationException",
        ),
    ] {
        let refused = list_mount_targets(router, &query).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (status, error),
            "case {case:?}: expected {status} {error}, got {} / {:?}",
            refused.status,
            refused.body
        );
    }
}

/// Rule 6: an update replaces the whole security-group set, and the replacement is what
/// every later read reports.
#[tokio::test]
async fn update_mount_target_replaces_the_security_groups() {
    let network = network().await;
    let router = &network.router;
    let file_system_id = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-update",
        Some("mt-update"),
    )
    .await;
    let subnet = &network.subnets[0];
    let created = created_mount_target(router, &file_system_id, &subnet.subnet_id).await;
    let mount_target_id = created.text("mountTargetId");
    let interface_id = created.text("networkInterfaceId");
    let path = format!("/mount-targets/{mount_target_id}");

    let first_group = network.security_group(&subnet.vpc_id, "s3files-mt-one").await;
    let second_group = network.security_group(&subnet.vpc_id, "s3files-mt-two").await;

    // The rows differ along what the rule generalises over: how many groups the write
    // names, whether the set grows or shrinks, and whether the groups were ever held
    // before. Each one is asserted through the owner of the interface too: the groups are
    // an attribute of an EC2 resource, so a replacement this service only recorded would
    // show up as a difference there.
    for (case, groups) in [
        ("two groups", vec![first_group.clone(), second_group.clone()]),
        ("one of them, so the other is taken away", vec![second_group.clone()]),
        ("the other one, so the set is replaced", vec![first_group.clone()]),
    ] {
        let updated = call(router, "PUT", &path, Some(json!({ "securityGroups": groups }))).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200, got {} / {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            text_list(updated.field("securityGroups")),
            groups,
            "case {case:?}: the update must report the set it stored, got {:?}",
            updated.body
        );
        // Everything else about the mount target is untouched by a security-group write.
        assert_eq!(
            updated.text("subnetId"),
            subnet.subnet_id,
            "case {case:?}: an update must not move the mount target, got {:?}",
            updated.body
        );

        let read = get_mount_target(router, &mount_target_id).await;
        assert_eq!(
            text_list(read.field("securityGroups")),
            groups,
            "case {case:?}: the read must report the replaced set, got {:?}",
            read.body
        );

        let interface = network
            .network_interface(&interface_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: EC2 must hold the interface"));
        assert_eq!(
            interface
                .groups()
                .iter()
                .filter_map(|group| group.group_id().map(str::to_string))
                .collect::<Vec<String>>(),
            groups,
            "case {case:?}: the interface must sit in the groups the update named, got \
             {:?}",
            interface.groups()
        );
    }

    // A set the owner of the interface will not apply is not one the mount target may
    // report: an interface always sits in at least one group ("You must specify at least
    // one group", `ModifyNetworkInterfaceAttribute$Groups`), so an empty replacement is
    // refused and the stored set is left exactly as it was.
    let held = text_list(
        get_mount_target(router, &mount_target_id)
            .await
            .field("securityGroups"),
    );
    let emptied = call(router, "PUT", &path, Some(json!({ "securityGroups": [] }))).await;
    assert_eq!(
        (emptied.status, emptied.error_type.as_str()),
        (400, "ValidationException"),
        "case \"no group at all\": an empty replacement must be refused, got {} / {:?}",
        emptied.status,
        emptied.body
    );
    assert_eq!(
        text_list(
            get_mount_target(router, &mount_target_id)
                .await
                .field("securityGroups")
        ),
        held,
        "case \"no group at all\": a refused update must change nothing, got {:?}",
        get_mount_target(router, &mount_target_id).await.body
    );

    // "All security groups must belong to the same VPC as the mount target's subnet": a
    // group EC2 reports in ANOTHER VPC is refused, and the stored set is untouched.
    let other_vpc = network.other_vpc().await;
    let foreign_group = network.security_group(&other_vpc, "s3files-mt-foreign").await;
    let refused = call(
        router,
        "PUT",
        &path,
        Some(json!({ "securityGroups": [foreign_group] })),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (400, "ValidationException"),
        "a group of another VPC must be refused, got {} / {:?}",
        refused.status,
        refused.body
    );
    assert_eq!(
        text_list(get_mount_target(router, &mount_target_id).await.field("securityGroups")),
        held,
        "a refused update must change nothing, got {:?}",
        get_mount_target(router, &mount_target_id).await.body
    );

    let unknown = call(
        router,
        "PUT",
        "/mount-targets/fsmt-0123456789abcdef0",
        Some(json!({ "securityGroups": [] })),
    )
    .await;
    assert_eq!(
        (unknown.status, unknown.error_type.as_str()),
        (404, "ResourceNotFoundException"),
        "updating a mount target this service does not hold must be not found, got {} / {:?}",
        unknown.status,
        unknown.body
    );
}

/// Rules 1 and 2, refusal half: a mount target is placed only where it can really be
/// placed, and only at an address the subnet can really give it.
#[tokio::test]
async fn a_create_is_refused_without_a_file_system_a_subnet_or_a_free_address() {
    let network = network().await;
    let router = &network.router;
    let file_system_id = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-refusals",
        Some("mt-refuse"),
    )
    .await;
    let subnet = &network.subnets[0];
    let taken = subnet.address_at(50);
    let held = create_mount_target(
        router,
        json!({
            "fileSystemId": &file_system_id,
            "subnetId": subnet.subnet_id,
            "ipv4Address": taken,
        }),
    )
    .await;
    assert_eq!(
        held.status, 200,
        "setup: the first mount target must be created, got {} / {:?}",
        held.status, held.body
    );
    // A second file system, so each refusal below is about the request and not about the
    // zone rule the first mount target already holds.
    let other = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-refusals-other",
        Some("mt-refuse-other"),
    )
    .await;

    for (case, request, status, error, message) in [
        (
            "a file system this service does not hold",
            json!({
                "fileSystemId": "fs-0123456789abcdef0",
                "subnetId": subnet.subnet_id,
            }),
            404,
            "ResourceNotFoundException",
            Some("File system does not exist."),
        ),
        (
            "a subnet the account does not have",
            json!({
                "fileSystemId": &other,
                "subnetId": "subnet-0123456789abcdef0",
            }),
            404,
            "ResourceNotFoundException",
            Some("Subnet does not exist."),
        ),
        (
            "an address outside the subnet's block",
            json!({
                "fileSystemId": &other,
                "subnetId": subnet.subnet_id,
                "ipv4Address": "203.0.113.7",
            }),
            400,
            "ValidationException",
            None,
        ),
        (
            "an address another mount target in the subnet holds",
            json!({
                "fileSystemId": &other,
                "subnetId": subnet.subnet_id,
                "ipv4Address": taken,
            }),
            409,
            "ConflictException",
            None,
        ),
        (
            // The default VPC has no IPv6 block, so there is no IPv6 address to give:
            // "The IP address type must match the IP configuration of the specified
            // subnet."
            "an IPv6-only mount target in a subnet with no IPv6 block",
            json!({
                "fileSystemId": &other,
                "subnetId": subnet.subnet_id,
                "ipAddressType": "IPV6_ONLY",
            }),
            400,
            "ValidationException",
            None,
        ),
    ] {
        let refused = create_mount_target(router, request).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (status, error),
            "case {case:?}: expected {status} {error}, got {} / {:?}",
            refused.status,
            refused.body
        );
        if let Some(message) = message {
            assert_eq!(
                refused.text("message"),
                message,
                "case {case:?}: expected the condition's own sentence, got {:?}",
                refused.body
            );
        }
        assert_eq!(
            listed_ids(&list_mount_targets(router, &format!("?fileSystemId={other}")).await),
            Vec::<String>::new(),
            "case {case:?}: a refused create must persist nothing"
        );
    }
}

/// Rule 8: a file system with a mount target is not deleted while that endpoint stands —
/// `forceDelete` does not lift it, because that member is about data pending export rather
/// than about the file system's resources — and the delete goes through once the mount
/// target has been deleted on its own.
#[tokio::test]
async fn delete_file_system_refuses_while_a_mount_target_exists() {
    let network = network().await;
    let router = &network.router;
    let file_system_id = created_file_system_id(
        router,
        "arn:aws:s3:::s3files-mount-parent",
        Some("mt-parent"),
    )
    .await;
    let mount_target_id = created_mount_target(router, &file_system_id, &network.subnets[0].subnet_id)
        .await
        .text("mountTargetId");

    // The rows differ along what the rule generalises over: whether `forceDelete` is
    // absent, false or true. None of them may remove a file system that still has a mount
    // target.
    for (case, query) in [
        ("absent", ""),
        ("false", "?forceDelete=false"),
        ("true", "?forceDelete=true"),
    ] {
        let refused = call(
            router,
            "DELETE",
            &format!("/file-systems/{file_system_id}{query}"),
            None,
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (409, "ConflictException"),
            "case \"forceDelete {case}\": a file system with a mount target must be refused, \
             got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.text("resourceId"),
            file_system_id,
            "case \"forceDelete {case}\": resourceId names the file system in use, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.text("message"),
            format!(
                "The file system '{file_system_id}' cannot be deleted because it has mount \
                 targets. You must delete the mount targets, and then try again."
            ),
            "case \"forceDelete {case}\": expected the condition's own sentence, got {:?}",
            refused.body
        );
        assert_eq!(
            get_mount_target(router, &mount_target_id).await.status,
            200,
            "case \"forceDelete {case}\": a refused delete must change nothing"
        );
    }

    // Once the endpoint is gone, the identical delete goes through.
    let released = call(
        router,
        "DELETE",
        &format!("/mount-targets/{mount_target_id}"),
        None,
    )
    .await;
    assert_eq!(
        released.status, 204,
        "DeleteMountTarget must be accepted, got {} / {:?}",
        released.status, released.body
    );
    let deleted = call(
        router,
        "DELETE",
        &format!("/file-systems/{file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "with no mount target left the delete must be accepted, got {} / {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        get_mount_target(router, &mount_target_id).await.status,
        404,
        "the deleted mount target stays gone"
    );
}
