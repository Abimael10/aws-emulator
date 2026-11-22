// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Availability Zones of a REGION, as this front door reports and
//! names them.
//!
//! RULE: the zones a Region exposes are the ones AWS publishes FOR THAT REGION — most expose
//! three, several expose more, and a Region's zone letters are not always the first letters
//! of the alphabet — so neither a placement nor a refusal may assume a fixed three. Two
//! answers rest on that list, and the rows below span Regions publishing three, four and six
//! zones, and one whose published letters skip a letter:
//!
//! 1. The zones a DB CLUSTER whose request named none is placed across: a cluster is "a
//!    writer and two reader DB instances … each in a different Availability Zone", so it
//!    spans THREE zones — the Region's first three, whatever the number that Region exposes.
//! 2. The zones the `InvalidVPCNetworkStateFault` of a Multi-AZ DB cluster names as the ones
//!    a caller can add a subnet in: EVERY zone of the Region its DB subnet group does not
//!    cover, comma-space separated. The count of zones in that list is what tells a caller
//!    how many subnets to add, so a Region with four zones and a group covering none names
//!    four, not one.
//!
//! The zone names are written out from the lists AWS publishes for these Regions ("Regions
//! and Zones", which `DescribeAvailabilityZones` answers with for each of them) rather than
//! read from the code under test, so the expectation is the published fact and not the
//! emulator's own table.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so the assertions
//! read the XML this emulator itself answered with.

use crate::db_instances::{call_in, emulator, expect_error, expect_ok, text, texts};

/// How many Availability Zones a DB cluster whose request named none is placed across
/// (`CreateDBCluster`: "the request creates a writer and two reader DB instances for you,
/// each in a different Availability Zone").
const CLUSTER_ZONES: usize = 3;

/// The settings `CreateDBCluster` documents as required for a Multi-AZ DB cluster, with an
/// `Iops` inside the ratio `Iops` documents against `AllocatedStorage`.
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "100"),
    ("DBClusterInstanceClass", "db.m6gd.xlarge"),
    ("StorageType", "io1"),
    ("Iops", "1000"),
];

/// One Region under test: its name and the Availability Zones AWS publishes for it.
struct RegionZones {
    case: &'static str,
    region: &'static str,
    zones: &'static [&'static str],
}

/// The Regions the rows run over: the oldest Region, which publishes six zones; one that
/// publishes four; one whose three published zones SKIP the second letter; and one that
/// publishes the three the alphabet starts with.
const REGIONS: &[RegionZones] = &[
    RegionZones {
        case: "a Region publishing six zones",
        region: "us-east-1",
        zones: &[
            "us-east-1a",
            "us-east-1b",
            "us-east-1c",
            "us-east-1d",
            "us-east-1e",
            "us-east-1f",
        ],
    },
    RegionZones {
        case: "a Region publishing four zones",
        region: "us-west-2",
        zones: &["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
    },
    RegionZones {
        case: "a Region whose published letters skip one",
        region: "ap-northeast-1",
        zones: &["ap-northeast-1a", "ap-northeast-1c", "ap-northeast-1d"],
    },
    RegionZones {
        case: "a Region publishing the first three letters",
        region: "eu-west-1",
        zones: &["eu-west-1a", "eu-west-1b", "eu-west-1c"],
    },
];

/// `CreateDBSubnetGroup` in `region` over `count` subnets of the published EC2 identifier
/// form.
///
/// No Amazon EC2 is mounted beside this service here, so the zone of each identifier is
/// unknown to it: the group covers NO zone of the Region, which is what makes every zone of
/// the Region one a caller could add a subnet in. (The suite that mounts the owner of subnets
/// beside this service is `db_subnet_group_ec2_zones`.)
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    region: &str,
    name: &str,
    count: usize,
) -> (u16, String) {
    let mut params: Vec<(String, String)> = vec![
        ("DBSubnetGroupName".to_string(), name.to_string()),
        (
            "DBSubnetGroupDescription".to_string(),
            "a group under test".to_string(),
        ),
    ];
    for index in 0..count {
        params.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            format!("subnet-{index:017x}"),
        ));
    }
    call_in(router, region, "CreateDBSubnetGroup", &params).await
}

#[tokio::test]
async fn a_cluster_the_request_placed_nowhere_spans_the_regions_first_three_zones() {
    let router = emulator();

    for row in REGIONS {
        assert!(
            row.zones.len() >= CLUSTER_ZONES,
            "case {:?}: every Region publishes at least three zones, got {:?}",
            row.case,
            row.zones
        );
        let identifier = format!("emu-zones-cl-{}", row.region);
        let params: Vec<(&str, &str)> = vec![
            ("DBClusterIdentifier", identifier.as_str()),
            ("Engine", "aurora-postgresql"),
            ("MasterUsername", "emuadmin"),
            ("MasterUserPassword", "emu-password-1"),
        ];
        expect_ok(
            row.case,
            call_in(&router, row.region, "CreateDBCluster", &params).await,
        );

        // Read back through the describe: the placement is what the store holds, not what
        // the create happened to answer.
        let reported = expect_ok(
            row.case,
            call_in(
                &router,
                row.region,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        let placed = texts(&reported, "AvailabilityZone");
        let expected: Vec<String> = row
            .zones
            .iter()
            .take(CLUSTER_ZONES)
            .map(|zone| (*zone).to_string())
            .collect();
        assert_eq!(
            placed, expected,
            "case {:?}: expected the Region's first {CLUSTER_ZONES} published zones, got \
             {placed:?} in {reported}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_multi_az_refusal_names_every_published_zone_the_group_does_not_cover() {
    let router = emulator();

    for row in REGIONS {
        let group = format!("emu-zones-group-{}", row.region);
        expect_ok(
            row.case,
            create_group(&router, row.region, &group, 2).await,
        );
        let identifier = format!("emu-zones-multiaz-{}", row.region);
        let mut params: Vec<(&str, &str)> = vec![
            ("DBClusterIdentifier", identifier.as_str()),
            ("Engine", "mysql"),
            ("DBSubnetGroupName", group.as_str()),
        ];
        params.extend(MULTI_AZ_SETTINGS.iter().copied());
        let body = expect_error(
            row.case,
            call_in(&router, row.region, "CreateDBCluster", &params).await,
            400,
            "InvalidVPCNetworkStateFault",
        );
        let message = text(&body, "Message");
        let expected = format!(
            "Can't create a Multi-AZ DB cluster because there aren't enough subnets \
             available. At least three subnets with available IP addresses in different \
             Availability Zones are required. You can add subnets to a DB subnet group in \
             the following Availability Zones: {}.",
            row.zones.join(", ")
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
        // Nothing was created: the cluster the refused create named is not there.
        expect_error(
            row.case,
            call_in(
                &router,
                row.region,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await,
            404,
            "DBClusterNotFoundFault",
        );
    }
}
