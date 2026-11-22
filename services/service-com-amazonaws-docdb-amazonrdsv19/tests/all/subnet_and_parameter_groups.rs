// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the subnet-group and cluster-parameter-group paths, and for the
//! two rules `CreateDBCluster` applies to what a request NAMES.
//!
//! Every test drives the service over its real wire protocol (awsQuery form bodies in, XML
//! out) through the same [`MultiServiceRouter`](smithy_server_runtime::MultiServiceRouter)
//! the production router uses, with the wire harness this target shares
//! (`tests/all/cluster_lifecycle.rs`): a create is always read back through a describe, and
//! a delete is always followed by the describe that must no longer find the resource, so a
//! response that is not computed from persisted state cannot pass.
//!
//! The rules asserted here, each over a TABLE spanning the class it generalises over:
//!
//! * a describe or delete of a named resource that state does not hold answers that
//!   resource's own modelled not-found fault, never an empty page and never a success;
//! * a create that NAMES another resource (`DBSubnetGroupName`,
//!   `DBClusterParameterGroupName`) is refused with that resource's modelled not-found
//!   fault unless the resource exists, and nothing is stored;
//! * an `EngineVersion` the engine does not offer is refused with awsQuery's documented
//!   `InvalidParameterCombination`, and one it does offer is stored resolved to a published
//!   version (`crate::state::DOCDB_ENGINE_VERSIONS`);
//! * a subnet group reports the placement the service that OWNS subnets holds for the
//!   subnets it names — the VPC, each subnet's Availability Zone and the network types they
//!   are addressed for — on the create, on the modify and on the read back, and a request
//!   naming a subnet the account does not hold, or subnets of two VPCs, is refused with
//!   nothing written. Those rows mount Amazon EC2 beside this service on ONE engine, so
//!   every subnet id in them is one EC2 minted;
//! * a subnet group's subnets must cover at least two of the Region's Availability Zones:
//!   a list covering fewer is refused with the modelled
//!   `DBSubnetGroupDoesNotCoverEnoughAZs` naming the coverage those subnets add up to, on
//!   the create and on the modify, with nothing written either way.

use service_com_amazonaws_docdb_amazonrdsv19::state;

use crate::cluster_lifecycle::{
    count, error_code, let_the_clock_catch_up, opt_text, text, texts, Docdb, REGION,
};

/// The two subnets a well-formed group needs. These identifiers name no subnet of any
/// account: the rows that use them mount this service ALONE, where the owner of subnets
/// cannot be asked, so they stand for subnets whose zones are unknown rather than for
/// subnets of a known zone (the Availability-Zone coverage rows below mount Amazon EC2 and
/// use ids it minted).
const SUBNET_A: &str = "subnet-0aaaaaaaaaaaaaaa1";
const SUBNET_B: &str = "subnet-0bbbbbbbbbbbbbbb2";
const SUBNET_C: &str = "subnet-0ccccccccccccccc3";

/// Create a well-formed subnet group named `name` over `subnets`, requiring a 200.
async fn create_group(docdb: &Docdb, case: &str, name: &str, subnets: &[&str]) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("DBSubnetGroupName", name),
        ("DBSubnetGroupDescription", "subnets for the test cluster"),
    ];
    // The awsQuery list form uses the list member's `xmlName`
    // (`SubnetIdentifierList$member` -> `SubnetIdentifier`), so the subnets arrive as
    // `SubnetIds.SubnetIdentifier.1=...`.
    let members: Vec<String> = (1..=subnets.len())
        .map(|index| format!("SubnetIds.SubnetIdentifier.{index}"))
        .collect();
    for (member, subnet) in members.iter().zip(subnets) {
        params.push((member.as_str(), subnet));
    }
    docdb.ok(case, "CreateDBSubnetGroup", &params).await
}

// ---------------------------------------------------------------------------
// The subnet-group lifecycle, end to end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_subnet_group_lifecycle_is_computed_from_persisted_state() {
    let docdb = Docdb::new();

    // The rows differ along what the rule generalises over: how many subnets the group
    // carries, the case of its name (stored lowercased), and its description.
    let rows: [(&str, &str, &[&str]); 3] = [
        ("two subnets", "test-subnets", &[SUBNET_A, SUBNET_B]),
        ("three subnets", "wider-subnets", &[SUBNET_A, SUBNET_B, SUBNET_C]),
        ("mixed-case name", "Mixed.Case_Name", &[SUBNET_B, SUBNET_C]),
    ];

    for (case, name, subnets) in rows {
        let created = create_group(&docdb, case, name, subnets).await;
        let stored_name = name.to_ascii_lowercase();
        assert_eq!(
            text(&created, "DBSubnetGroupName"),
            stored_name,
            "case {case:?}: `DBSubnetGroupName` is stored as a lowercase string, got {created}"
        );
        assert_eq!(
            text(&created, "DBSubnetGroupArn"),
            format!("arn:aws:rds:{REGION}:000000000000:subgrp:{stored_name}"),
            "case {case:?}: expected the regional subnet-group ARN, got {created}"
        );
        assert_eq!(
            texts(&created, "SubnetIdentifier"),
            subnets
                .iter()
                .map(|subnet| (*subnet).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the subnets the request named, got {created}"
        );

        // Read the create back through the describe: same name, same subnets, same ARN.
        let described = docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name)],
            )
            .await;
        assert_eq!(
            count(&described, "DBSubnetGroup"),
            1,
            "case {case:?}: a named describe must report exactly that group, got {described}"
        );
        assert_eq!(
            text(&described, "DBSubnetGroupName"),
            stored_name,
            "case {case:?}: expected the stored group, got {described}"
        );
        assert_eq!(
            texts(&described, "SubnetIdentifier").len(),
            subnets.len(),
            "case {case:?}: expected {} subnets read back, got {described}",
            subnets.len()
        );
        assert_eq!(
            text(&described, "DBSubnetGroupDescription"),
            "subnets for the test cluster",
            "case {case:?}: expected the stored description, got {described}"
        );
    }

    // The unnamed describe reports every stored group, in one page.
    let all = docdb.ok("list all", "DescribeDBSubnetGroups", &[]).await;
    assert_eq!(
        count(&all, "DBSubnetGroup"),
        rows.len(),
        "case \"list all\": expected every created group, got {all}"
    );
    assert_eq!(
        opt_text(&all, "Marker"),
        None,
        "case \"list all\": a single page carries no Marker, got {all}"
    );

    // Deleting a group removes it: the delete answers 200 and the describe that named it
    // now answers the modelled 404, which is what a waiter polls for.
    for (case, name, _) in rows {
        let deleted = docdb
            .ok(case, "DeleteDBSubnetGroup", &[("DBSubnetGroupName", name)])
            .await;
        assert!(
            deleted.contains("DeleteDBSubnetGroupResponse"),
            "case {case:?}: expected the DeleteDBSubnetGroupResponse envelope, got {deleted}"
        );

        let gone = docdb
            .call("DescribeDBSubnetGroups", &[("DBSubnetGroupName", name)])
            .await;
        assert_eq!(
            (gone.status, error_code(&gone.xml).as_str()),
            (404, "DBSubnetGroupNotFoundFault"),
            "case {case:?}: a deleted group must report the modelled not-found fault, got {} \
             with {}",
            gone.status,
            gone.xml
        );
    }

    let empty = docdb
        .ok("list after deletes", "DescribeDBSubnetGroups", &[])
        .await;
    assert_eq!(
        count(&empty, "DBSubnetGroup"),
        0,
        "case \"list after deletes\": every group was deleted, got {empty}"
    );
}

/// A named resource state does not hold answers that resource's own modelled not-found
/// fault — on the read, on the delete, and whatever shape the name takes.
#[tokio::test]
async fn a_subnet_group_that_does_not_exist_is_the_modelled_not_found_fault() {
    let docdb = Docdb::new();
    create_group(&docdb, "seed", "present-group", &[SUBNET_A, SUBNET_B]).await;

    // (case, the name the request gives) — spanning a plain name, a name that differs only
    // in case from nothing stored, an ARN form, and the reserved `default` group this
    // emulator seeds no record for.
    let missing: [(&str, &str); 4] = [
        ("a name never created", "no-such-group"),
        ("a near miss of a stored name", "present-group-2"),
        (
            "an ARN of a group that does not exist",
            "arn:aws:rds:us-east-1:000000000000:subgrp:no-such-group",
        ),
        ("the reserved default group", "default"),
    ];

    for (case, name) in missing {
        for action in ["DescribeDBSubnetGroups", "DeleteDBSubnetGroup"] {
            let reply = docdb.call(action, &[("DBSubnetGroupName", name)]).await;
            assert_eq!(
                (reply.status, error_code(&reply.xml).as_str()),
                (404, "DBSubnetGroupNotFoundFault"),
                "case {case:?} on {action}: expected 404 DBSubnetGroupNotFoundFault, got {} \
                 with {}",
                reply.status,
                reply.xml
            );
        }
    }

    // The group that DOES exist is untouched by every refusal above.
    let still_there = docdb
        .ok(
            "the stored group survives",
            "DescribeDBSubnetGroups",
            &[("DBSubnetGroupName", "present-group")],
        )
        .await;
    assert_eq!(
        text(&still_there, "DBSubnetGroupName"),
        "present-group",
        "case \"the stored group survives\": expected the stored group, got {still_there}"
    );
}

/// `CreateDBSubnetGroup`'s own documented refusals, each with the modelled fault.
#[tokio::test]
async fn creating_a_subnet_group_holds_its_documented_rules() {
    let docdb = Docdb::new();
    create_group(&docdb, "seed", "taken-name", &[SUBNET_A, SUBNET_B]).await;

    // (case, name, subnets, expected status, expected wire code)
    let rows: [(&str, &str, &[&str], u16, &str); 5] = [
        (
            // "Must not be default" — the model states it on the member itself, and the
            // model declares no error shape for it, so it is the boundary refusal.
            "the reserved name",
            "default",
            &[SUBNET_A, SUBNET_B],
            400,
            "ValidationException",
        ),
        (
            "a name outside the documented character set",
            "bad/name",
            &[SUBNET_A, SUBNET_B],
            400,
            "ValidationException",
        ),
        (
            // "subnet groups must contain at least one subnet in at least two
            // Availability Zones".
            "one subnet",
            "too-narrow",
            &[SUBNET_A],
            400,
            "DBSubnetGroupDoesNotCoverEnoughAZs",
        ),
        (
            // The same subnet twice is one subnet, so it cannot cover two zones either.
            "the same subnet twice",
            "still-too-narrow",
            &[SUBNET_A, SUBNET_A],
            400,
            "DBSubnetGroupDoesNotCoverEnoughAZs",
        ),
        (
            "a name already in use",
            "taken-name",
            &[SUBNET_A, SUBNET_B],
            400,
            // The model's `awsQueryError` code for `DBSubnetGroupAlreadyExistsFault`.
            "DBSubnetGroupAlreadyExists",
        ),
    ];

    for (case, name, subnets, expected_status, expected_code) in rows {
        let mut params: Vec<(&str, &str)> = vec![
            ("DBSubnetGroupName", name),
            ("DBSubnetGroupDescription", "a group the service refuses"),
        ];
        let members: Vec<String> = (1..=subnets.len())
            .map(|index| format!("SubnetIds.SubnetIdentifier.{index}"))
            .collect();
        for (member, subnet) in members.iter().zip(subnets) {
            params.push((member.as_str(), subnet));
        }
        let reply = docdb.call("CreateDBSubnetGroup", &params).await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected {expected_status} {expected_code}, got {} with {}",
            reply.status,
            reply.xml
        );
    }

    // Nothing a refusal named reached the store: only the seeded group is there.
    let all = docdb
        .ok("only the seeded group is stored", "DescribeDBSubnetGroups", &[])
        .await;
    assert_eq!(
        texts(&all, "DBSubnetGroupName"),
        vec!["taken-name".to_string()],
        "case \"only the seeded group is stored\": a refused create must store nothing, got \
         {all}"
    );
}

/// The two published quotas this store enforces, each with the modelled fault: more subnets
/// in one group than the per-group quota, and more groups than the per-Region quota.
#[tokio::test]
async fn the_subnet_group_quotas_refuse_a_store_that_would_grow_past_them() {
    let docdb = Docdb::new();

    // `state::MAX_SUBNETS_PER_SUBNET_GROUP` subnets is the published bound and is accepted;
    // one past it is the modelled fault. The ids are generated so the row follows the
    // constant rather than a literal count.
    let within: Vec<String> = (0..state::MAX_SUBNETS_PER_SUBNET_GROUP)
        .map(|index| format!("subnet-{index:017x}"))
        .collect();
    let past: Vec<String> = (0..=state::MAX_SUBNETS_PER_SUBNET_GROUP)
        .map(|index| format!("subnet-{index:017x}"))
        .collect();

    for (case, subnets, expected) in [
        ("at the published per-group bound", &within, None),
        (
            "one subnet past the per-group bound",
            &past,
            // This fault carries no `awsQueryError` override, so the wire code is the
            // shape name.
            Some("DBSubnetQuotaExceededFault"),
        ),
    ] {
        let name = format!("quota-{}", subnets.len());
        let borrowed: Vec<&str> = subnets.iter().map(String::as_str).collect();
        let mut params: Vec<(&str, &str)> = vec![
            ("DBSubnetGroupName", name.as_str()),
            ("DBSubnetGroupDescription", "a group at the quota"),
        ];
        let members: Vec<String> = (1..=borrowed.len())
            .map(|index| format!("SubnetIds.SubnetIdentifier.{index}"))
            .collect();
        for (member, subnet) in members.iter().zip(&borrowed) {
            params.push((member.as_str(), subnet));
        }
        let reply = docdb.call("CreateDBSubnetGroup", &params).await;
        match expected {
            None => assert_eq!(
                reply.status, 200,
                "case {case:?}: the published bound must be accepted, got {} with {}",
                reply.status, reply.xml
            ),
            Some(code) => assert_eq!(
                (reply.status, error_code(&reply.xml).as_str()),
                (400, code),
                "case {case:?}: expected 400 {code}, got {} with {}",
                reply.status,
                reply.xml
            ),
        }
    }

    // The per-Region quota: the store fills to `state::MAX_DB_SUBNET_GROUPS` and the next
    // create is the modelled fault rather than a stored record.
    let seeded = count(
        &docdb
            .ok("count the stored groups", "DescribeDBSubnetGroups", &[])
            .await,
        "DBSubnetGroup",
    );
    for index in seeded..state::MAX_DB_SUBNET_GROUPS {
        let name = format!("filler-{index}");
        create_group(&docdb, "fill the Region", &name, &[SUBNET_A, SUBNET_B]).await;
    }
    let over = docdb
        .call(
            "CreateDBSubnetGroup",
            &[
                ("DBSubnetGroupName", "one-too-many"),
                ("DBSubnetGroupDescription", "past the Region quota"),
                ("SubnetIds.SubnetIdentifier.1", SUBNET_A),
                ("SubnetIds.SubnetIdentifier.2", SUBNET_B),
            ],
        )
        .await;
    assert_eq!(
        (over.status, error_code(&over.xml).as_str()),
        // The model's `awsQueryError` code for `DBSubnetGroupQuotaExceededFault`.
        (400, "DBSubnetGroupQuotaExceeded"),
        "case \"one group past the Region quota\": expected 400 DBSubnetGroupQuotaExceeded, \
         got {} with {}",
        over.status,
        over.xml
    );
    let stored = count(
        &docdb
            .ok("the store stops at the quota", "DescribeDBSubnetGroups", &[])
            .await,
        "DBSubnetGroup",
    );
    assert_eq!(
        stored,
        state::MAX_DB_SUBNET_GROUPS,
        "case \"the store stops at the quota\": expected the store to hold exactly the \
         quota, got {stored}"
    );
}

/// A subnet group a cluster is placed in is in use: the delete is refused with the modelled
/// state fault and the record stays, until the cluster is gone.
///
/// The cluster names the group by its ARN, which the member accepts: the record must keep
/// the RESOLVED name, or the reference is invisible to the in-use rule and the cluster
/// reports a name the group is not stored under.
#[tokio::test]
async fn a_subnet_group_in_use_by_a_cluster_cannot_be_deleted() {
    let docdb = Docdb::new();
    let created = create_group(&docdb, "seed", "in-use-group", &[SUBNET_A, SUBNET_B]).await;
    let group_arn = text(&created, "DBSubnetGroupArn");
    let placed = docdb
        .ok(
            "place a cluster in the group, naming it by ARN",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "placed-cluster"),
                ("Engine", state::DOCDB_ENGINE),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "secret99"),
                ("DBSubnetGroupName", &group_arn),
            ],
        )
        .await;
    assert_eq!(
        text(&placed, "DBSubnetGroup"),
        "in-use-group",
        "case \"place a cluster in the group, naming it by ARN\": the cluster must report the \
         group's stored name, not the form the request used, got {placed}"
    );

    let refused = docdb
        .call(
            "DeleteDBSubnetGroup",
            &[("DBSubnetGroupName", "in-use-group")],
        )
        .await;
    assert_eq!(
        (refused.status, error_code(&refused.xml).as_str()),
        (400, "InvalidDBSubnetGroupStateFault"),
        "case \"in use\": expected 400 InvalidDBSubnetGroupStateFault, got {} with {}",
        refused.status,
        refused.xml
    );

    let still_there = docdb
        .ok(
            "the group survives the refusal",
            "DescribeDBSubnetGroups",
            &[("DBSubnetGroupName", "in-use-group")],
        )
        .await;
    assert_eq!(
        text(&still_there, "DBSubnetGroupName"),
        "in-use-group",
        "case \"the group survives the refusal\": expected the stored group, got {still_there}"
    );

    // With the cluster gone the same delete succeeds, so the refusal really was the
    // reference and not a blanket refusal.
    let_the_clock_catch_up();
    docdb
        .ok(
            "drop the cluster",
            "DeleteDBCluster",
            &[
                ("DBClusterIdentifier", "placed-cluster"),
                ("SkipFinalSnapshot", "true"),
            ],
        )
        .await;
    docdb
        .ok(
            "delete the freed group",
            "DeleteDBSubnetGroup",
            &[("DBSubnetGroupName", "in-use-group")],
        )
        .await;
}

// ---------------------------------------------------------------------------
// The placement a subnet group reports, resolved against the owner of subnets
// ---------------------------------------------------------------------------

/// The IPv6 address pool the dual-stack rows below bring a VPC's IPv6 block from
/// (`AssociateVpcCidrBlockRequest$Ipv6Pool`: "The ID of an IPv6 address pool from which to
/// allocate the IPv6 CIDR block").
const IPV6_POOL: &str = "ipv6pool-ec2-0123456789abcdef0";

/// A well-formed subnet id for a subnet NOBODY minted: the `subnet-` plus 17 hexadecimal
/// characters form EC2 reports its own ids in, so a refusal of it is about the account not
/// holding the subnet rather than about the spelling.
const UNHELD_SUBNET_A: &str = "subnet-00000000000000001";
/// A second one; see [`UNHELD_SUBNET_A`].
const UNHELD_SUBNET_B: &str = "subnet-00000000000000002";

/// The account's network for the rows below: Amazon DocumentDB and Amazon EC2 — the service
/// that OWNS subnets — on ONE engine, with the VPCs and subnets a client minted through EC2
/// itself.
///
/// Every subnet id below is one EC2 MINTED and every expected zone and VPC is one EC2
/// reported, so no value in an assertion is a value this service could have invented.
struct Network {
    docdb: Docdb,
    ec2: ::aws_sdk_ec2::Client,
}

impl Network {
    /// Both services on one engine, with an EC2 client over the router's own in-process
    /// callback.
    fn new() -> Self {
        let (docdb, ec2) = Docdb::with_ec2();
        Self { docdb, ec2 }
    }

    /// The Availability Zones of [`REGION`] the account has, as EC2 reports them — asked,
    /// never assumed, so a subnet below is placed in a zone the Region really has.
    async fn zones(&self) -> Vec<String> {
        let reported = self
            .ec2
            .describe_availability_zones()
            .send()
            .await
            .expect("EC2 must report the Region's Availability Zones");
        let mut zones: Vec<String> = reported
            .availability_zones()
            .iter()
            .filter_map(|zone| zone.zone_name().map(str::to_string))
            .filter(|zone| zone.starts_with(REGION))
            .collect();
        zones.sort();
        assert!(
            zones.len() >= 3,
            "case \"the account's zones\": the Region must publish at least three zones, got \
             {zones:?}"
        );
        zones
    }

    /// A VPC with `cidr_block`, and with `ipv6_cidr_block` associated when one is given.
    async fn vpc(&self, cidr_block: &str, ipv6_cidr_block: Option<&str>) -> String {
        let mut create = self.ec2.create_vpc().cidr_block(cidr_block);
        if let Some(block) = ipv6_cidr_block {
            create = create.ipv6_cidr_block(block).ipv6_pool(IPV6_POOL);
        }
        create
            .send()
            .await
            .unwrap_or_else(|error| panic!("EC2 must create the VPC {cidr_block}: {error:?}"))
            .vpc()
            .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
            .expect("CreateVpc must report the VPC it minted")
            .to_string()
    }

    /// A subnet of `vpc_id` with `cidr_block` in `zone`, and with `ipv6_cidr_block`
    /// associated when one is given — the dual-stack case.
    async fn subnet(
        &self,
        vpc_id: &str,
        cidr_block: &str,
        zone: &str,
        ipv6_cidr_block: Option<&str>,
    ) -> String {
        let subnet_id = self
            .ec2
            .create_subnet()
            .vpc_id(vpc_id)
            .cidr_block(cidr_block)
            .availability_zone(zone)
            .send()
            .await
            .unwrap_or_else(|error| panic!("EC2 must create a subnet in {zone}: {error:?}"))
            .subnet()
            .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
            .expect("CreateSubnet must report the subnet it minted")
            .to_string();
        if let Some(block) = ipv6_cidr_block {
            self.ec2
                .associate_subnet_cidr_block()
                .subnet_id(&subnet_id)
                .ipv6_cidr_block(block)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("EC2 must associate {block} with {subnet_id}: {error:?}")
                });
        }
        subnet_id
    }

    /// The Availability Zone EC2 reports for `subnet_id` — read off the owner's record, not
    /// derived from the id or from what this service answered.
    async fn zone_of(&self, subnet_id: &str) -> String {
        self.ec2
            .describe_subnets()
            .subnet_ids(subnet_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("EC2 must report the subnet {subnet_id}: {error:?}"))
            .subnets()
            .iter()
            .find(|subnet| subnet.subnet_id() == Some(subnet_id))
            .and_then(::aws_sdk_ec2::types::Subnet::availability_zone)
            .unwrap_or_else(|| panic!("EC2 must report a zone for {subnet_id}"))
            .to_string()
    }

    /// Remove `subnet_id` from the account, so the identifier names no subnet any more.
    async fn delete_subnet(&self, subnet_id: &str) {
        self.ec2
            .delete_subnet()
            .subnet_id(subnet_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("EC2 must delete {subnet_id}: {error:?}"));
    }
}

/// The awsQuery members a subnet-group create or modify carries for `subnets`, as owned
/// pairs the harness borrows from.
///
/// The list form uses the list member's `xmlName` (`SubnetIdentifierList$member` ->
/// `SubnetIdentifier`), so the subnets arrive as `SubnetIds.SubnetIdentifier.<n>`.
fn group_members(
    name: &str,
    description: Option<&str>,
    subnets: &[String],
) -> Vec<(String, String)> {
    let mut members = vec![("DBSubnetGroupName".to_string(), name.to_string())];
    if let Some(description) = description {
        members.push((
            "DBSubnetGroupDescription".to_string(),
            description.to_string(),
        ));
    }
    for (index, subnet) in subnets.iter().enumerate() {
        members.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.clone(),
        ));
    }
    members
}

/// `group_members` as the harness takes them.
fn borrowed(members: &[(String, String)]) -> Vec<(&str, &str)> {
    members
        .iter()
        .map(|(member, value)| (member.as_str(), value.as_str()))
        .collect()
}

/// What one `DBSubnetGroup` element of a response says about its placement.
#[derive(Debug, PartialEq, Eq)]
struct WirePlacement {
    vpc_id: Option<String>,
    /// Each subnet, in the order the group reports them, with the zone it reports.
    subnets: Vec<(String, String)>,
    network_types: Vec<String>,
}

/// The placement the one `DBSubnetGroup` in `xml` reports.
///
/// `DBSubnetGroup` carries exactly one list of plain strings (`SupportedNetworkTypes`) and
/// one `Name` element per subnet (inside `SubnetAvailabilityZone`), so the shared reader's
/// per-tag lists line up with the subnets in document order — which is why every row below
/// names subnets that DO resolve to a zone.
fn wire_placement(case: &str, xml: &str) -> WirePlacement {
    let identifiers = texts(xml, "SubnetIdentifier");
    let zones = texts(xml, "Name");
    assert_eq!(
        identifiers.len(),
        zones.len(),
        "case {case:?}: every subnet of the group must report its Availability Zone, got {xml}"
    );
    WirePlacement {
        vpc_id: opt_text(xml, "VpcId"),
        subnets: identifiers.into_iter().zip(zones).collect(),
        network_types: texts(xml, "member"),
    }
}

/// The placement these subnets must be reported with: the VPC and the zones EC2 holds for
/// them, in the order the request named them, plus the network types the group offers.
async fn expected_placement(
    net: &Network,
    vpc_id: &str,
    subnets: &[String],
    network_types: &[&str],
) -> WirePlacement {
    let mut reported = Vec::with_capacity(subnets.len());
    for subnet in subnets {
        reported.push((subnet.clone(), net.zone_of(subnet).await));
    }
    WirePlacement {
        vpc_id: Some(vpc_id.to_string()),
        subnets: reported,
        network_types: network_types.iter().map(|kind| (*kind).to_string()).collect(),
    }
}

/// A group reports, for every subnet it holds, the VPC and the Availability Zone the service
/// that OWNS subnets reports it in, and the network types those subnets are addressed for —
/// on the create and on the read back.
///
/// The rows differ along what the rule generalises over: how many subnets the group holds,
/// the order they are named in, which VPC they sit in, and the address families EC2
/// addresses them in (IPv4-only, dual-stack, and a group mixing the two).
#[tokio::test]
async fn a_group_reports_the_placement_ec2_holds_for_the_subnets_it_names() {
    let net = Network::new();
    let zones = net.zones().await;

    let ipv4_vpc = net.vpc("10.61.0.0/16", None).await;
    let mut ipv4_subnets = Vec::new();
    for (index, zone) in zones.iter().take(3).enumerate() {
        ipv4_subnets.push(
            net.subnet(&ipv4_vpc, &format!("10.61.{}.0/24", index + 1), zone, None)
                .await,
        );
    }
    // A second VPC, this one with an IPv6 block of its own: two of its subnets carry a /64
    // out of it and the third is addressed in IPv4 alone.
    let dual_vpc = net.vpc("10.62.0.0/16", Some("2001:db8:62::/56")).await;
    let dual_a = net
        .subnet(
            &dual_vpc,
            "10.62.1.0/24",
            &zones[0],
            Some("2001:db8:62:1::/64"),
        )
        .await;
    let dual_b = net
        .subnet(
            &dual_vpc,
            "10.62.2.0/24",
            &zones[1],
            Some("2001:db8:62:2::/64"),
        )
        .await;
    let ipv4_only_in_dual_vpc = net
        .subnet(&dual_vpc, "10.62.3.0/24", &zones[2], None)
        .await;

    // (case, the subnets the request names, the VPC they lie in, the network types the
    // group must offer). The two values are the ones `DBSubnetGroup$SupportedNetworkTypes`
    // publishes — "Valid Values: IPV4 | DUAL" — and `DUAL` is offered only where EVERY
    // subnet of the group carries both families.
    let rows: [(&str, Vec<String>, &str, &[&str]); 5] = [
        (
            "two subnets of one VPC",
            vec![ipv4_subnets[0].clone(), ipv4_subnets[1].clone()],
            &ipv4_vpc,
            &["IPV4"],
        ),
        (
            "three subnets of one VPC",
            ipv4_subnets.clone(),
            &ipv4_vpc,
            &["IPV4"],
        ),
        (
            "the same three in another order",
            vec![
                ipv4_subnets[2].clone(),
                ipv4_subnets[0].clone(),
                ipv4_subnets[1].clone(),
            ],
            &ipv4_vpc,
            &["IPV4"],
        ),
        (
            "two dual-stack subnets of another VPC",
            vec![dual_a.clone(), dual_b.clone()],
            &dual_vpc,
            &["IPV4", "DUAL"],
        ),
        (
            "a dual-stack subnet beside an IPv4-only one",
            vec![dual_a.clone(), ipv4_only_in_dual_vpc.clone()],
            &dual_vpc,
            &["IPV4"],
        ),
    ];

    for (index, (case, subnets, vpc_id, network_types)) in rows.iter().enumerate() {
        let name = format!("placement-{index}");
        let expected = expected_placement(&net, vpc_id, subnets, network_types).await;
        let members = group_members(&name, Some("a group of resolved subnets"), subnets);
        let created = net
            .docdb
            .ok(case, "CreateDBSubnetGroup", &borrowed(&members))
            .await;
        // The create's answer and a later read of the same group report the same placement:
        // it is persisted, not echoed.
        let described = net
            .docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name.as_str())],
            )
            .await;
        for (reader, xml) in [("create", &created), ("describe", &described)] {
            let reported = wire_placement(case, xml);
            assert_eq!(
                reported, expected,
                "case {case:?} / {reader}: expected the VPC, zones and network types EC2 \
                 holds for these subnets, got {xml}"
            );
        }
    }
}

/// A modify re-resolves the list it is given: the group's reported placement is the one EC2
/// holds for its NEW subnets, not the one it was created with.
///
/// The rows differ along what the rule generalises over: how many subnets the new list
/// holds, the order they are named in, the address families EC2 addresses them in, and
/// whether the request also carries a description. They all stay inside the group's own VPC,
/// which is the only list a modify may name (the sibling suite spans the refusal for one
/// that does not).
#[tokio::test]
async fn a_modify_rewrites_the_placement_from_the_subnets_it_names() {
    let net = Network::new();
    let zones = net.zones().await;

    // One VPC with an IPv6 block of its own, so the rows can change which address families
    // the group's subnets carry without leaving the VPC the group belongs to.
    let vpc = net.vpc("10.63.0.0/16", Some("2001:db8:63::/56")).await;
    let ipv4_a = net.subnet(&vpc, "10.63.1.0/24", &zones[0], None).await;
    let ipv4_b = net.subnet(&vpc, "10.63.2.0/24", &zones[1], None).await;
    let dual_a = net
        .subnet(&vpc, "10.63.3.0/24", &zones[2], Some("2001:db8:63:3::/64"))
        .await;
    let dual_b = net
        .subnet(&vpc, "10.63.4.0/24", &zones[0], Some("2001:db8:63:4::/64"))
        .await;

    // One row: the case, the new subnets, the network types the group then offers, and the
    // description the modify carries.
    type ModifyRow = (
        &'static str,
        Vec<String>,
        &'static [&'static str],
        Option<&'static str>,
    );

    let rows: [ModifyRow; 4] = [
        (
            "a wider list of IPv4-only subnets",
            vec![ipv4_a.clone(), ipv4_b.clone(), dual_a.clone()],
            &["IPV4"],
            None,
        ),
        (
            "two dual-stack subnets",
            vec![dual_a.clone(), dual_b.clone()],
            &["IPV4", "DUAL"],
            None,
        ),
        (
            "the same two in the other order, with a new description",
            vec![dual_b.clone(), dual_a.clone()],
            &["IPV4", "DUAL"],
            Some("the edited description"),
        ),
        (
            "a narrower list, back to IPv4 only",
            vec![ipv4_b.clone(), dual_a.clone()],
            &["IPV4"],
            None,
        ),
    ];

    for (index, (case, subnets, network_types, description)) in rows.iter().enumerate() {
        let name = format!("modified-{index}");
        // Every row starts from a group of the two IPv4-only subnets, so what the modify
        // reports can only come from the list it was given.
        let seed = group_members(&name, Some("the created description"), &[
            ipv4_a.clone(),
            ipv4_b.clone(),
        ]);
        net.docdb
            .ok(case, "CreateDBSubnetGroup", &borrowed(&seed))
            .await;

        let expected = expected_placement(&net, &vpc, subnets, network_types).await;
        let members = group_members(&name, *description, subnets);
        let modified = net
            .docdb
            .ok(case, "ModifyDBSubnetGroup", &borrowed(&members))
            .await;
        let described = net
            .docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name.as_str())],
            )
            .await;
        for (reader, xml) in [("modify", &modified), ("describe", &described)] {
            let reported = wire_placement(case, xml);
            assert_eq!(
                reported, expected,
                "case {case:?} / {reader}: expected the placement EC2 holds for the new \
                 subnets, got {xml}"
            );
            assert_eq!(
                text(xml, "DBSubnetGroupDescription"),
                description.unwrap_or("the created description"),
                "case {case:?} / {reader}: a modify carrying no description leaves the \
                 stored one alone, got {xml}"
            );
        }
    }
}

/// The message AWS refuses a `SubnetIds` list it cannot use with, written here from the
/// recorded template rather than from the handler's own helper: the fixed text
/// "Some input subnets in :[" … "] are invalid." around the subnet ids the REQUEST named,
/// comma-separated in the order it named them — the sentence says that SOME of the subnets
/// it was asked about are invalid, so the ones that did resolve are named too.
fn expected_invalid_subnets_message(subnet_ids: &[String]) -> String {
    format!(
        "Some input subnets in :[{}] are invalid.",
        subnet_ids.join(", ")
    )
}

/// A `SubnetIds` member the account has no subnet for names no subnet at all, so the
/// request is refused with awsQuery's `InvalidParameterValue` quoting the subnets the
/// request named — all of them, the ones that resolved included — and nothing is written.
///
/// The rows differ along what the rule generalises over: how many identifiers do not
/// resolve, where in the list they sit, whether one is repeated, and whether the identifier
/// never existed or named a subnet the account has since removed.
#[tokio::test]
async fn a_subnet_the_account_does_not_hold_is_refused_and_nothing_is_stored() {
    let net = Network::new();
    let zones = net.zones().await;

    let vpc = net.vpc("10.65.0.0/16", None).await;
    let held_a = net.subnet(&vpc, "10.65.1.0/24", &zones[0], None).await;
    let held_b = net.subnet(&vpc, "10.65.2.0/24", &zones[1], None).await;
    // A subnet the account HAD: removed through the service that owns it, so the identifier
    // stops resolving exactly as a typo'd one does.
    let removed = net.subnet(&vpc, "10.65.3.0/24", &zones[2], None).await;
    net.delete_subnet(&removed).await;

    // (case, the subnets the request names, the identifiers the refusal must name — the
    // request's own list, each named once, in request order)
    let rows: [(&str, Vec<String>, Vec<String>); 5] = [
        (
            "an unheld id in second place",
            vec![held_a.clone(), UNHELD_SUBNET_A.to_string()],
            vec![held_a.clone(), UNHELD_SUBNET_A.to_string()],
        ),
        (
            "an unheld id first",
            vec![UNHELD_SUBNET_A.to_string(), held_a.clone()],
            vec![UNHELD_SUBNET_A.to_string(), held_a.clone()],
        ),
        (
            "two unheld ids",
            vec![UNHELD_SUBNET_B.to_string(), UNHELD_SUBNET_A.to_string()],
            // In the order the request named them.
            vec![UNHELD_SUBNET_B.to_string(), UNHELD_SUBNET_A.to_string()],
        ),
        (
            "the same unheld id twice",
            vec![
                held_a.clone(),
                UNHELD_SUBNET_A.to_string(),
                UNHELD_SUBNET_A.to_string(),
            ],
            // One subnet named twice is one subnet, so it is named once.
            vec![held_a.clone(), UNHELD_SUBNET_A.to_string()],
        ),
        (
            "a subnet the account removed",
            vec![held_a.clone(), removed.clone()],
            vec![held_a.clone(), removed.clone()],
        ),
    ];

    for (index, (case, subnets, named)) in rows.iter().enumerate() {
        let name = format!("unheld-{index}");
        let members = group_members(&name, Some("a group of unheld subnets"), subnets);
        let reply = net
            .docdb
            .call("CreateDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (400, "InvalidParameterValue"),
            "case {case:?}: expected 400 InvalidParameterValue, got {} with {}",
            reply.status,
            reply.xml
        );
        assert_eq!(
            text(&reply.xml, "Message"),
            expected_invalid_subnets_message(named),
            "case {case:?}: the refusal names the subnets the request asked about, got {}",
            reply.xml
        );

        let stored = net
            .docdb
            .call("DescribeDBSubnetGroups", &[("DBSubnetGroupName", &name)])
            .await;
        assert_eq!(
            (stored.status, error_code(&stored.xml).as_str()),
            (404, "DBSubnetGroupNotFoundFault"),
            "case {case:?}: a refused create must store nothing, got {} with {}",
            stored.status,
            stored.xml
        );
    }

    // The same rule on the modify path: the stored group keeps the subnets it had.
    let name = "modify-unheld";
    let seed = group_members(
        name,
        Some("a group of held subnets"),
        &[held_a.clone(), held_b.clone()],
    );
    net.docdb
        .ok("seed the modify row", "CreateDBSubnetGroup", &borrowed(&seed))
        .await;
    let members = group_members(
        name,
        None,
        &[held_a.clone(), UNHELD_SUBNET_A.to_string()],
    );
    let reply = net
        .docdb
        .call("ModifyDBSubnetGroup", &borrowed(&members))
        .await;
    assert_eq!(
        (reply.status, error_code(&reply.xml).as_str()),
        (400, "InvalidParameterValue"),
        "case \"modify to an unheld subnet\": expected 400 InvalidParameterValue, got {} with \
         {}",
        reply.status,
        reply.xml
    );
    assert_eq!(
        text(&reply.xml, "Message"),
        expected_invalid_subnets_message(&[held_a.clone(), UNHELD_SUBNET_A.to_string()]),
        "case \"modify to an unheld subnet\": the refusal names the subnets the request asked \
         about, got {}",
        reply.xml
    );
    let described = net
        .docdb
        .ok(
            "the group survives the refused modify",
            "DescribeDBSubnetGroups",
            &[("DBSubnetGroupName", name)],
        )
        .await;
    assert_eq!(
        texts(&described, "SubnetIdentifier"),
        vec![held_a.clone(), held_b.clone()],
        "case \"modify to an unheld subnet\": a refused modify changes nothing, got {described}"
    );
}

/// A subnet group holds the subnets of ONE VPC. On the create, a list whose subnets lie in
/// two is the modelled `InvalidSubnet`, worded with the sentence that fault itself states.
/// On the modify that one VPC is the group's OWN, so a list naming a subnet of another VPC
/// is refused as awsQuery's `InvalidParameterValue` naming the subnets the request asked
/// about. Either way nothing is written.
#[tokio::test]
async fn subnets_of_two_vpcs_cannot_make_one_group() {
    let net = Network::new();
    let zones = net.zones().await;

    let left_vpc = net.vpc("10.66.0.0/16", None).await;
    let left_a = net.subnet(&left_vpc, "10.66.1.0/24", &zones[0], None).await;
    let left_b = net.subnet(&left_vpc, "10.66.2.0/24", &zones[1], None).await;
    let right_vpc = net.vpc("10.67.0.0/16", None).await;
    let right_a = net
        .subnet(&right_vpc, "10.67.1.0/24", &zones[0], None)
        .await;
    let right_b = net
        .subnet(&right_vpc, "10.67.2.0/24", &zones[1], None)
        .await;

    // The rows differ in the order the two VPCs are named in and in how many subnets each
    // contributes.
    let rows: [(&str, Vec<String>); 3] = [
        (
            "one subnet from each VPC",
            vec![left_a.clone(), right_a.clone()],
        ),
        (
            "the same two in the other order",
            vec![right_a.clone(), left_a.clone()],
        ),
        (
            "two from one VPC and one from the other",
            vec![left_a.clone(), left_b.clone(), right_b.clone()],
        ),
    ];

    for (index, (case, subnets)) in rows.iter().enumerate() {
        let name = format!("two-vpcs-{index}");
        let members = group_members(&name, Some("a group spanning two VPCs"), subnets);
        let reply = net
            .docdb
            .call("CreateDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            // The model's `awsQueryError` code for `InvalidSubnet`.
            (400, "InvalidSubnet"),
            "case {case:?}: expected 400 InvalidSubnet, got {} with {}",
            reply.status,
            reply.xml
        );
        assert_eq!(
            text(&reply.xml, "Message"),
            // The sentence `InvalidSubnet`'s own documentation states, which carries no
            // identifier: the condition is about the list, not about one subnet of it.
            "The requested subnet is not valid, or multiple subnets were requested that are \
             not all in a common virtual private cloud (VPC).",
            "case {case:?}: expected the sentence the fault states, got {}",
            reply.xml
        );

        let stored = net
            .docdb
            .call("DescribeDBSubnetGroups", &[("DBSubnetGroupName", &name)])
            .await;
        assert_eq!(
            (stored.status, error_code(&stored.xml).as_str()),
            (404, "DBSubnetGroupNotFoundFault"),
            "case {case:?}: a refused create must store nothing, got {} with {}",
            stored.status,
            stored.xml
        );
    }

    // The same rule on the modify path, and one step stricter there: the group belongs to
    // the VPC it was built in, so a list that names a subnet of ANOTHER VPC is refused even
    // when every subnet it names lies in one VPC of its own — and the refusal names the
    // subnets the request asked about, as the unheld-subnet refusal does.
    let rows: [(&str, Vec<String>, Vec<String>); 2] = [
        (
            "a modify list straddling two VPCs",
            vec![left_a.clone(), right_b.clone()],
            vec![left_a.clone(), right_b.clone()],
        ),
        (
            "a modify list wholly inside another VPC",
            vec![right_a.clone(), right_b.clone()],
            vec![right_a.clone(), right_b.clone()],
        ),
    ];
    for (index, (case, subnets, named)) in rows.iter().enumerate() {
        let name = format!("modify-two-vpcs-{index}");
        let seed = group_members(
            &name,
            Some("a group of one VPC's subnets"),
            &[left_a.clone(), left_b.clone()],
        );
        net.docdb
            .ok(case, "CreateDBSubnetGroup", &borrowed(&seed))
            .await;
        let members = group_members(&name, None, subnets);
        let reply = net
            .docdb
            .call("ModifyDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (400, "InvalidParameterValue"),
            "case {case:?}: expected 400 InvalidParameterValue, got {} with {}",
            reply.status,
            reply.xml
        );
        assert_eq!(
            text(&reply.xml, "Message"),
            expected_invalid_subnets_message(named),
            "case {case:?}: the refusal names the subnets the request asked about, got {}",
            reply.xml
        );
        let described = net
            .docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name.as_str())],
            )
            .await;
        assert_eq!(
            texts(&described, "SubnetIdentifier"),
            vec![left_a.clone(), left_b.clone()],
            "case {case:?}: a refused modify changes nothing, got {described}"
        );
    }
}

/// `ModifyDBSubnetGroupMessage$DBSubnetGroupName`: "Must match the name of an existing
/// DBSubnetGroup" — a name (or ARN) that matches none is the modelled 404, worded as the
/// mutating paths of this front door word it.
#[tokio::test]
async fn modifying_a_subnet_group_that_does_not_exist_is_the_modelled_not_found_fault() {
    let net = Network::new();
    let zones = net.zones().await;
    let vpc = net.vpc("10.68.0.0/16", None).await;
    let held = vec![
        net.subnet(&vpc, "10.68.1.0/24", &zones[0], None).await,
        net.subnet(&vpc, "10.68.2.0/24", &zones[1], None).await,
    ];

    // (case, the name the modify gives)
    let rows: [(&str, &str); 3] = [
        ("a name never created", "no-such-group"),
        (
            "an ARN of a group that does not exist",
            "arn:aws:rds:us-east-1:000000000000:subgrp:no-such-group",
        ),
        ("a near miss of a stored name", "held-groups"),
    ];
    let seed = group_members(
        "held-group",
        Some("a group that does exist"),
        &held,
    );
    net.docdb
        .ok("seed", "CreateDBSubnetGroup", &borrowed(&seed))
        .await;

    for (case, requested) in rows {
        let members = group_members(requested, None, &held);
        let reply = net
            .docdb
            .call("ModifyDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (404, "DBSubnetGroupNotFoundFault"),
            "case {case:?}: expected 404 DBSubnetGroupNotFoundFault, got {} with {}",
            reply.status,
            reply.xml
        );
        assert_eq!(
            text(&reply.xml, "Message"),
            format!("DB subnet group '{requested}' does not exist."),
            "case {case:?}: expected the mutating path's wording, got {}",
            reply.xml
        );
    }

    // The stored group is still what it was: a refused modify of another name touched it
    // not at all.
    let described = net
        .docdb
        .ok(
            "the stored group survives",
            "DescribeDBSubnetGroups",
            &[("DBSubnetGroupName", "held-group")],
        )
        .await;
    assert_eq!(
        texts(&described, "SubnetIdentifier"),
        held,
        "case \"the stored group survives\": expected the subnets it was created with, got \
         {described}"
    );
}

/// `DBSubnetGroup$SupportedNetworkTypes`: "Valid Values: IPV4 | DUAL ... A DBSubnetGroup can
/// support only the IPv4 protocol or the IPv4 and the IPv6 protocols (DUAL)." Which of them
/// a group offers follows from the address families EVERY one of its subnets is addressed
/// in, because a cluster is placed across all of them.
#[test]
fn the_network_types_a_group_offers_follow_from_its_subnets_address_families() {
    // (case, every subnet carries IPv4, every subnet carries IPv6, the published values)
    let rows: [(&str, bool, bool, &[&str]); 4] = [
        ("IPv4 everywhere", true, false, &["IPV4"]),
        ("both families everywhere", true, true, &["IPV4", "DUAL"]),
        // Both published values include IPv4, so a group a subnet carries no IPv4 block in
        // offers neither rather than an invented one.
        ("IPv6 only", false, true, &[]),
        ("no block at all", false, false, &[]),
    ];
    for (case, ipv4, ipv6, expected) in rows {
        let offered = state::group_network_types(ipv4, ipv6);
        assert_eq!(
            offered, expected,
            "case {case:?}: expected {expected:?}, got {offered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The Availability Zones a group's subnets have to add up to
// ---------------------------------------------------------------------------

/// The `DBSubnetGroupDoesNotCoverEnoughAZs` sentence pair for `coverage`, written here from
/// AWS's own template rather than from the handler's helper: the fixed text around the
/// zones the request's subnets COVER, comma-separated in zone order, and the number of
/// zones a group must cover.
fn expected_az_coverage_message(coverage: &[&str]) -> String {
    format!(
        "The DB subnet group doesn't meet Availability Zone (AZ) coverage requirement. \
         Current AZ coverage: {}. Add subnets to cover at least {} AZs.",
        coverage.join(", "),
        state::MIN_SUBNET_GROUP_AVAILABILITY_ZONES
    )
}

/// `CreateDBSubnetGroup`: "subnet groups must contain at least one subnet in at least two
/// Availability Zones in the Amazon Web Services Region." A list whose subnets cover fewer
/// is the modelled `DBSubnetGroupDoesNotCoverEnoughAZs`, whose message names the zones those
/// subnets DO cover — read off the service that owns subnets — and nothing is stored; a list
/// that covers two or more is accepted.
///
/// The rows differ along what the rule generalises over: how many subnets share the one
/// zone, which zone of the Region that is, whether a subnet is named twice, and — on the
/// accepted side — how many subnets and zones the group spans and in which order they are
/// named. Every zone named in an expectation is one EC2 reported for the subnet, so no value
/// here is one this service could have invented.
#[tokio::test]
async fn a_group_whose_subnets_cover_one_zone_is_refused_naming_that_coverage() {
    let net = Network::new();
    let zones = net.zones().await;

    let vpc = net.vpc("10.69.0.0/16", None).await;
    // Three subnets of the Region's FIRST zone, two of its second, one of its third: enough
    // to build a list of any width inside one zone, and lists spanning two or three.
    let first_a = net.subnet(&vpc, "10.69.1.0/24", &zones[0], None).await;
    let first_b = net.subnet(&vpc, "10.69.2.0/24", &zones[0], None).await;
    let first_c = net.subnet(&vpc, "10.69.3.0/24", &zones[0], None).await;
    let second_a = net.subnet(&vpc, "10.69.4.0/24", &zones[1], None).await;
    let second_b = net.subnet(&vpc, "10.69.5.0/24", &zones[1], None).await;
    let third_a = net.subnet(&vpc, "10.69.6.0/24", &zones[2], None).await;

    // (case, the subnets the request names, the zones the refusal must name)
    let refused: [(&str, Vec<String>, Vec<String>); 6] = [
        (
            "two subnets of the first zone",
            vec![first_a.clone(), first_b.clone()],
            vec![net.zone_of(&first_a).await],
        ),
        (
            "three subnets of the first zone",
            vec![first_a.clone(), first_b.clone(), first_c.clone()],
            vec![net.zone_of(&first_a).await],
        ),
        (
            "a single subnet",
            vec![first_a.clone()],
            vec![net.zone_of(&first_a).await],
        ),
        (
            // One subnet named twice is one subnet, so it covers the one zone it lies in.
            "the same subnet twice",
            vec![first_a.clone(), first_a.clone()],
            vec![net.zone_of(&first_a).await],
        ),
        (
            // Another zone of the Region: the coverage the message names comes from the
            // subnets, not from a zone written into the service.
            "two subnets of the second zone",
            vec![second_a.clone(), second_b.clone()],
            vec![net.zone_of(&second_a).await],
        ),
        (
            "a single subnet of the third zone",
            vec![third_a.clone()],
            vec![net.zone_of(&third_a).await],
        ),
    ];

    for (index, (case, subnets, coverage)) in refused.iter().enumerate() {
        let name = format!("one-zone-{index}");
        let members = group_members(&name, Some("a group inside one zone"), subnets);
        let reply = net
            .docdb
            .call("CreateDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            // The model's `awsQueryError` code for the fault.
            (400, "DBSubnetGroupDoesNotCoverEnoughAZs"),
            "case {case:?}: expected 400 DBSubnetGroupDoesNotCoverEnoughAZs, got {} with {}",
            reply.status,
            reply.xml
        );
        let named: Vec<&str> = coverage.iter().map(String::as_str).collect();
        assert_eq!(
            text(&reply.xml, "Message"),
            expected_az_coverage_message(&named),
            "case {case:?}: the refusal names the coverage these subnets add up to, got {}",
            reply.xml
        );

        let stored = net
            .docdb
            .call("DescribeDBSubnetGroups", &[("DBSubnetGroupName", &name)])
            .await;
        assert_eq!(
            (stored.status, error_code(&stored.xml).as_str()),
            (404, "DBSubnetGroupNotFoundFault"),
            "case {case:?}: a refused create must store nothing, got {} with {}",
            stored.status,
            stored.xml
        );
    }

    // The positive control: a list covering two or more zones is accepted, reports the
    // subnets it named and the zones EC2 holds for them, and reads back the same. Every
    // subnet of this VPC is addressed in IPv4 alone, so every row offers `IPV4`; which
    // network types a group offers is the sibling rows' rule, not this one's.
    let accepted: [(&str, Vec<String>); 4] = [
        (
            "one subnet in each of two zones",
            vec![first_a.clone(), second_a.clone()],
        ),
        (
            "two subnets of one zone beside one of another",
            vec![first_a.clone(), first_b.clone(), second_a.clone()],
        ),
        (
            "three zones, named last to first",
            vec![third_a.clone(), second_a.clone(), first_a.clone()],
        ),
        (
            "two subnets of the second and third zones",
            vec![second_b.clone(), third_a.clone()],
        ),
    ];

    for (index, (case, subnets)) in accepted.iter().enumerate() {
        let name = format!("two-zones-{index}");
        let expected = expected_placement(&net, &vpc, subnets, &["IPV4"]).await;
        let members = group_members(&name, Some("a group spanning zones"), subnets);
        let created = net
            .docdb
            .ok(case, "CreateDBSubnetGroup", &borrowed(&members))
            .await;
        let described = net
            .docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name.as_str())],
            )
            .await;
        for (reader, xml) in [("create", &created), ("describe", &described)] {
            assert_eq!(
                wire_placement(case, xml),
                expected,
                "case {case:?} / {reader}: expected the placement EC2 holds for these \
                 subnets, got {xml}"
            );
            assert_eq!(
                text(xml, "SubnetGroupStatus"),
                state::SUBNET_GROUP_COMPLETE,
                "case {case:?} / {reader}: a stored group is complete, got {xml}"
            );
        }
    }
}

/// The same coverage rule on the modify path: a new list covering fewer than two zones is
/// refused with the fault naming the coverage it would have had, the stored group is left
/// exactly as it was, and a list that does cover two is applied.
///
/// The rows differ along how many subnets the refused list carries, which single zone they
/// share, and whether one of them is the subnet the group already holds.
#[tokio::test]
async fn a_modify_to_a_one_zone_list_is_refused_and_changes_nothing() {
    let net = Network::new();
    let zones = net.zones().await;

    let vpc = net.vpc("10.70.0.0/16", None).await;
    let first_a = net.subnet(&vpc, "10.70.1.0/24", &zones[0], None).await;
    let first_b = net.subnet(&vpc, "10.70.2.0/24", &zones[0], None).await;
    let second_a = net.subnet(&vpc, "10.70.3.0/24", &zones[1], None).await;
    let second_b = net.subnet(&vpc, "10.70.4.0/24", &zones[1], None).await;
    let third_a = net.subnet(&vpc, "10.70.5.0/24", &zones[2], None).await;

    // (case, the new list the modify names, the zones the refusal must name)
    let refused: [(&str, Vec<String>, Vec<String>); 4] = [
        (
            "both subnets in the zone the group already covers",
            vec![first_a.clone(), first_b.clone()],
            vec![net.zone_of(&first_a).await],
        ),
        (
            "a single subnet",
            vec![second_a.clone()],
            vec![net.zone_of(&second_a).await],
        ),
        (
            "two subnets of the second zone",
            vec![second_a.clone(), second_b.clone()],
            vec![net.zone_of(&second_a).await],
        ),
        (
            "the same subnet named twice",
            vec![third_a.clone(), third_a.clone()],
            vec![net.zone_of(&third_a).await],
        ),
    ];

    for (index, (case, subnets, coverage)) in refused.iter().enumerate() {
        let name = format!("modify-one-zone-{index}");
        let held = vec![first_a.clone(), second_a.clone()];
        let seed = group_members(&name, Some("a group spanning two zones"), &held);
        net.docdb
            .ok(case, "CreateDBSubnetGroup", &borrowed(&seed))
            .await;

        let members = group_members(&name, None, subnets);
        let reply = net
            .docdb
            .call("ModifyDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (400, "DBSubnetGroupDoesNotCoverEnoughAZs"),
            "case {case:?}: expected 400 DBSubnetGroupDoesNotCoverEnoughAZs, got {} with {}",
            reply.status,
            reply.xml
        );
        let named: Vec<&str> = coverage.iter().map(String::as_str).collect();
        assert_eq!(
            text(&reply.xml, "Message"),
            expected_az_coverage_message(&named),
            "case {case:?}: the refusal names the coverage the new list adds up to, got {}",
            reply.xml
        );

        let described = net
            .docdb
            .ok(
                case,
                "DescribeDBSubnetGroups",
                &[("DBSubnetGroupName", name.as_str())],
            )
            .await;
        assert_eq!(
            texts(&described, "SubnetIdentifier"),
            held,
            "case {case:?}: a refused modify changes nothing, got {described}"
        );

        // And a list that does cover two zones is applied to that same group, so the
        // refusals above are about the coverage rather than about the group.
        let widened = vec![first_b.clone(), third_a.clone()];
        let expected = expected_placement(&net, &vpc, &widened, &["IPV4"]).await;
        let members = group_members(&name, None, &widened);
        let modified = net
            .docdb
            .ok(case, "ModifyDBSubnetGroup", &borrowed(&members))
            .await;
        assert_eq!(
            wire_placement(case, &modified),
            expected,
            "case {case:?}: a list covering two zones is applied, got {modified}"
        );
    }
}

// ---------------------------------------------------------------------------
// What a create NAMES, and the engine-version catalogue
// ---------------------------------------------------------------------------

/// `CreateDBCluster` names two other resources by name. Each must exist, or the create is
/// refused with THAT resource's modelled not-found fault and nothing is stored.
#[tokio::test]
async fn creating_a_cluster_requires_every_resource_it_names_to_exist() {
    let docdb = Docdb::new();
    create_group(&docdb, "seed", "cluster-subnets", &[SUBNET_A, SUBNET_B]).await;
    // The default cluster parameter group of the engine's newest family, which exists in
    // every account and Region; see `state::default_parameter_groups`.
    let default_group = state::default_parameter_group_name(&state::parameter_group_family(
        state::DEFAULT_ENGINE_VERSION,
    ));

    // One row of the refusal table: the case, the extra members the create carries, and
    // the status and wire code the reference it names must be refused with.
    type Refusal = (&'static str, Vec<(&'static str, String)>, u16, &'static str);

    // (case, extra members, expected status, expected wire code)
    let refusals: [Refusal; 4] = [
        (
            "a subnet group that does not exist",
            vec![("DBSubnetGroupName", "no-such-group".to_string())],
            404,
            "DBSubnetGroupNotFoundFault",
        ),
        (
            "a subnet group named by an ARN that does not exist",
            vec![(
                "DBSubnetGroupName",
                "arn:aws:rds:us-east-1:000000000000:subgrp:no-such-group".to_string(),
            )],
            404,
            "DBSubnetGroupNotFoundFault",
        ),
        (
            "a cluster parameter group that does not exist",
            vec![(
                "DBClusterParameterGroupName",
                "no-such-parameter-group".to_string(),
            )],
            404,
            // The model's `awsQueryError` code for
            // `DBClusterParameterGroupNotFoundFault`.
            "DBClusterParameterGroupNotFound",
        ),
        (
            "an existing subnet group and a parameter group that does not exist",
            vec![
                ("DBSubnetGroupName", "cluster-subnets".to_string()),
                ("DBClusterParameterGroupName", "default.docdb9.9".to_string()),
            ],
            404,
            "DBClusterParameterGroupNotFound",
        ),
    ];

    for (case, extra, expected_status, expected_code) in refusals {
        let mut params: Vec<(&str, &str)> = vec![
            ("DBClusterIdentifier", "refused-cluster"),
            ("Engine", state::DOCDB_ENGINE),
            ("MasterUsername", "admin"),
            ("MasterUserPassword", "secret99"),
        ];
        for (member, value) in &extra {
            params.push((member, value.as_str()));
        }
        let reply = docdb.call("CreateDBCluster", &params).await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected {expected_status} {expected_code}, got {} with {}",
            reply.status,
            reply.xml
        );

        let stored = docdb
            .call(
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "refused-cluster")],
            )
            .await;
        assert_eq!(
            (stored.status, error_code(&stored.xml).as_str()),
            (404, "DBClusterNotFoundFault"),
            "case {case:?}: a refused create must store nothing, got {} with {}",
            stored.status,
            stored.xml
        );
    }

    // Both references resolved: the create succeeds and reports what it was given.
    let created = docdb
        .ok(
            "both references exist",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "placed-cluster"),
                ("Engine", state::DOCDB_ENGINE),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "secret99"),
                ("DBSubnetGroupName", "cluster-subnets"),
                ("DBClusterParameterGroupName", &default_group),
            ],
        )
        .await;
    assert_eq!(
        text(&created, "DBSubnetGroup"),
        "cluster-subnets",
        "case \"both references exist\": expected the subnet group it was placed in, got \
         {created}"
    );
    assert_eq!(
        text(&created, "DBClusterParameterGroup"),
        default_group,
        "case \"both references exist\": expected the parameter group it was given, got \
         {created}"
    );
}

/// The engine-version rule: a version the engine offers is accepted and stored resolved to
/// a published version; any other version is refused with awsQuery's documented
/// `InvalidParameterCombination`.
#[tokio::test]
async fn an_engine_version_outside_the_catalogue_is_refused() {
    let docdb = Docdb::new();

    // Every published version, named in full and as a line, plus the absent case. The
    // catalogue is `state::engine_versions`; the rows below are built from it so a
    // version added there is covered here too, and `4.0.0` — a version this issue did not
    // cite — is among them.
    let published_versions = state::engine_versions();
    for (index, published) in published_versions.iter().enumerate() {
        let line = published
            .rsplit_once('.')
            .map(|(line, _)| line.to_string())
            .expect("every published version carries a major.minor line");
        // A version named as a LINE asks for that line's own default release, which the
        // engine catalogue designates per release — not the newest patch the line publishes.
        let line_default = published_versions
            .iter()
            .rev()
            .filter(|candidate| candidate.starts_with(&format!("{line}.")))
            .find(|candidate| ::service_common::docdb_engine_catalog::is_line_default(candidate))
            .copied()
            .expect("every published line designates one of its releases its default");
        for (shape, requested, expected) in [
            ("in full", (*published).to_string(), *published),
            ("as a line", line.clone(), line_default),
        ] {
            let case = format!("{published} {shape}");
            let identifier = format!("version-{index}-{}", shape.replace(' ', "-"));
            let created = docdb
                .ok(
                    &case,
                    "CreateDBCluster",
                    &[
                        ("DBClusterIdentifier", &identifier),
                        ("Engine", state::DOCDB_ENGINE),
                        ("MasterUsername", "admin"),
                        ("MasterUserPassword", "secret99"),
                        ("EngineVersion", &requested),
                    ],
                )
                .await;
            assert_eq!(
                text(&created, "EngineVersion"),
                expected,
                "case {case:?}: a version named as a line resolves to the release that line \
                 designates as its default, got {created}"
            );
            // And it is the STORED version, not just the create's answer.
            let described = docdb
                .ok(
                    &case,
                    "DescribeDBClusters",
                    &[("DBClusterIdentifier", &identifier)],
                )
                .await;
            assert_eq!(
                text(&described, "EngineVersion"),
                expected,
                "case {case:?}: expected the stored version, got {described}"
            );
        }
    }

    let defaulted = docdb
        .ok(
            "no version at all",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "version-default"),
                ("Engine", state::DOCDB_ENGINE),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "secret99"),
            ],
        )
        .await;
    assert_eq!(
        text(&defaulted, "EngineVersion"),
        state::DEFAULT_ENGINE_VERSION,
        "case \"no version at all\": expected the documented default, got {defaulted}"
    );

    // (case, the version the request names) — one past each published bound, a line that
    // does not exist, a patch that does not exist, and values that are not versions.
    let refused: [(&str, &str); 6] = [
        ("far past the newest major", "99.9.9"),
        ("one past the newest major", "9.0.0"),
        ("one before the oldest major", "2.6.0"),
        ("a minor line the engine does not offer", "5.1"),
        ("a patch the engine does not publish", "5.0.9"),
        ("not a version at all", "latest"),
    ];

    for (case, version) in refused {
        let reply = docdb
            .call(
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", "refused-version"),
                    ("Engine", state::DOCDB_ENGINE),
                    ("MasterUsername", "admin"),
                    ("MasterUserPassword", "secret99"),
                    ("EngineVersion", version),
                ],
            )
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (400, "InvalidParameterCombination"),
            "case {case:?}: expected 400 InvalidParameterCombination, got {} with {}",
            reply.status,
            reply.xml
        );
        assert!(
            reply.xml.contains(version),
            "case {case:?}: the message is built from the request, so it must name the \
             version asked for, got {}",
            reply.xml
        );

        let stored = docdb
            .call(
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "refused-version")],
            )
            .await;
        assert_eq!(
            (stored.status, error_code(&stored.xml).as_str()),
            (404, "DBClusterNotFoundFault"),
            "case {case:?}: a refused create must store nothing, got {} with {}",
            stored.status,
            stored.xml
        );
    }

    // The same rule on the modify path, against a cluster that exists.
    let_the_clock_catch_up();
    let modified = docdb
        .call(
            "ModifyDBCluster",
            &[
                ("DBClusterIdentifier", "version-default"),
                ("EngineVersion", "99.9.9"),
                ("AllowMajorVersionUpgrade", "true"),
            ],
        )
        .await;
    assert_eq!(
        (modified.status, error_code(&modified.xml).as_str()),
        (400, "InvalidParameterCombination"),
        "case \"modify to a version outside the catalogue\": expected 400 \
         InvalidParameterCombination, got {} with {}",
        modified.status,
        modified.xml
    );
}

// ---------------------------------------------------------------------------
// Cluster parameter groups
// ---------------------------------------------------------------------------

/// The published engine-version catalogue is ordered oldest first, which is the order every
/// report publishes it in, and the engine's documented default is one of its rows — the
/// release the catalogue designates, not necessarily the last row.
#[test]
fn the_engine_version_catalogue_is_ordered_oldest_first() {
    let published = state::engine_versions();
    let mut sorted = published.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted, published,
        "case \"catalogue order\": the published versions must be listed oldest first, got \
         {published:?}"
    );
    assert!(
        published.contains(&state::DEFAULT_ENGINE_VERSION),
        "case \"catalogue order\": the documented default is a published version, got {:?} in \
         {published:?}",
        state::DEFAULT_ENGINE_VERSION
    );
}

/// Which parameter-group names and families THIS model may be asked about, which is what
/// the shared `rds` front door reads to decide whether to serve a parameter-group request
/// from it rather than from the RDS model's own handler.
#[test]
fn the_model_answers_only_for_its_own_parameter_group_families() {
    // (case, value, served by this model)
    let names: [(&str, &str, bool); 6] = [
        ("a default group of this engine", "default.docdb5.0", true),
        ("the oldest published family", "default.docdb3.6", true),
        ("a family this engine does not have", "default.docdb9.9", false),
        ("another engine's family", "default.aurora-mysql8.0", false),
        ("a caller-created group", "team-cluster-params", true),
        ("a name that merely starts like a default", "defaults.docdb5.0", true),
    ];
    for (case, name, served) in names {
        assert_eq!(
            state::serves_parameter_group_name(name),
            served,
            "case {case:?} ({name}): expected served == {served}"
        );
    }

    let families: [(&str, &str, bool); 4] = [
        ("this engine's newest family", "docdb5.0", true),
        ("this engine's oldest family", "docdb3.6", true),
        ("a family this engine does not have", "docdb9.9", false),
        ("another engine's family", "aurora-mysql8.0", false),
    ];
    for (case, family, served) in families {
        assert_eq!(
            state::serves_parameter_group_family(family),
            served,
            "case {case:?} ({family}): expected served == {served}"
        );
    }
}

/// `DescribeDBClusterParameterGroups` reports the default group of every engine-version
/// family, and a name that matches none of them is the modelled 404 — the answer a waiter
/// polls a deleted group for.
#[tokio::test]
async fn cluster_parameter_groups_report_the_engine_families_and_404_anything_else() {
    let docdb = Docdb::new();

    let all = docdb
        .ok("list all", "DescribeDBClusterParameterGroups", &[])
        .await;
    // One default group per engine-version family AWS publishes for this engine, named
    // literally rather than through the same function the handler answers from, and sorted
    // as the describe reports them.
    let expected = vec![
        "default.docdb3.6".to_string(),
        "default.docdb4.0".to_string(),
        "default.docdb5.0".to_string(),
        "default.docdb8.0".to_string(),
    ];
    assert_eq!(
        texts(&all, "DBClusterParameterGroupName"),
        expected,
        "case \"list all\": expected one default group per engine-version family, got {all}"
    );

    // Each one reads back on its own, with the family and ARN the name implies.
    for name in &expected {
        let described = docdb
            .ok(
                name,
                "DescribeDBClusterParameterGroups",
                &[("DBClusterParameterGroupName", name)],
            )
            .await;
        assert_eq!(
            count(&described, "DBClusterParameterGroup"),
            1,
            "case {name:?}: a named describe must report exactly that group, got {described}"
        );
        let family = text(&described, "DBParameterGroupFamily");
        assert_eq!(
            text(&described, "DBClusterParameterGroupName"),
            state::default_parameter_group_name(&family),
            "case {name:?}: the default group of a family is named after it, got {described}"
        );
        assert_eq!(
            text(&described, "DBClusterParameterGroupArn"),
            format!("arn:aws:rds:{REGION}:000000000000:cluster-pg:{name}"),
            "case {name:?}: expected the regional parameter-group ARN, got {described}"
        );
    }

    // (case, the name the request gives) — a name never created, a near miss of a default
    // group's name, a family that does not exist, and an engine that is not this one.
    let missing: [(&str, &str); 4] = [
        ("a name never created", "no-such-parameter-group"),
        ("a near miss of a default group", "default.docdb5"),
        ("a family the engine does not have", "default.docdb9.9"),
        ("another engine's family", "default.aurora5.6"),
    ];

    for (case, name) in missing {
        let reply = docdb
            .call(
                "DescribeDBClusterParameterGroups",
                &[("DBClusterParameterGroupName", name)],
            )
            .await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (404, "DBParameterGroupNotFound"),
            "case {case:?}: expected 404 DBParameterGroupNotFound (the model's awsQueryError \
             code for DBParameterGroupNotFoundFault), got {} with {}",
            reply.status,
            reply.xml
        );
    }
}
