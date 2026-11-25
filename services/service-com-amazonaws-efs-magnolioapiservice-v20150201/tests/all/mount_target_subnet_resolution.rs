// SPDX-License-Identifier: CC0-1.0

//! RULE: a mount target is PLACED in the subnet the request names, so `CreateMountTarget`
//! resolves `SubnetId` against the service that OWNS subnets and takes every placement fact
//! it reports — `VpcId`, `AvailabilityZoneName`, `AvailabilityZoneId` and the address — off
//! that subnet's record, persisting them so every later read serves the same values; a
//! `SubnetId` the account has no subnet for resolves to nothing, which is refused with
//! `SubnetNotFound` (400) before anything is written, so the file system keeps no mount
//! target it never got.
//!
//! The rows below differ along what that rule generalises over: which VPC the subnet belongs
//! to (the default one and two this suite's EC2 mints), which Availability Zone it is in, and
//! — for the refusal — the FORM of an identifier that resolves to nothing (the narrowest and
//! the widest the modelled pattern admits, one that differs from a real id by a single digit,
//! and one EC2 minted and then deleted). Every expected placement value is read from EC2's
//! own answer for that subnet, never written out here.

use serde_json::json;

use crate::harness::{matches_id_pattern, Emulator, SubnetFacts};

/// `CreateMountTarget$SubnetId` is `length 15..47, pattern ^subnet-[0-9a-f]{8,40}$` (this
/// service's Smithy model, shape `SubnetId`), so these are the narrowest and the widest
/// identifiers the boundary lets through to the resolution the rule is about.
const NARROWEST_SUBNET_ID: &str = "subnet-00000000";
const WIDEST_SUBNET_ID: &str = "subnet-ffffffffffffffffffffffffffffffffffffffff";

/// One subnet EC2 mints in a VPC of its own, so a row's expected VPC is a different one from
/// the default VPC's and from the other rows'.
async fn subnet_in_a_new_vpc(
    emulator: &Emulator,
    vpc_cidr: &str,
    subnet_cidr: &str,
    zone: &str,
) -> SubnetFacts {
    let ec2 = emulator.ec2();
    let vpc = ec2
        .create_vpc()
        .cidr_block(vpc_cidr)
        .send()
        .await
        .expect("EC2 must create a VPC")
        .vpc
        .and_then(|vpc| vpc.vpc_id)
        .expect("the created VPC must report its id");
    let subnet = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block(subnet_cidr)
        .availability_zone(zone)
        .send()
        .await
        .expect("EC2 must create a subnet")
        .subnet
        .expect("the created subnet must be reported");
    SubnetFacts {
        subnet_id: subnet.subnet_id.clone().expect("a subnet id"),
        vpc_id: subnet.vpc_id.clone().expect("a VPC id"),
        zone_name: subnet.availability_zone.clone().expect("a zone name"),
        availability_zone_id: subnet.availability_zone_id.clone().expect("a zone id"),
        cidr_block: subnet.cidr_block.clone().expect("a CIDR block"),
    }
}

/// RULE: the VPC, the zone and the address a mount target reports are the resolved subnet's
/// own, and a later `DescribeMountTargets` serves exactly those same values.
#[tokio::test]
async fn a_mount_target_reports_and_keeps_the_placement_of_its_subnet() {
    let efs = Emulator::new();
    let default_subnets = efs.account_subnets().await;
    assert!(
        default_subnets.len() >= 2,
        "the account must have at least two subnets to span zones, got {default_subnets:?}"
    );
    let first_zone = default_subnets[0].zone_name.clone();
    let second_zone = default_subnets[1].zone_name.clone();

    let cases: Vec<(&str, SubnetFacts)> = vec![
        ("a default-VPC subnet in the first zone", default_subnets[0].clone()),
        ("a default-VPC subnet in another zone", default_subnets[1].clone()),
        (
            "a subnet of a VPC of this account, in the first zone",
            subnet_in_a_new_vpc(&efs, "10.94.0.0/16", "10.94.3.0/24", &first_zone).await,
        ),
        (
            "a subnet of a second VPC, in another zone and a narrower block",
            subnet_in_a_new_vpc(&efs, "10.95.0.0/16", "10.95.8.0/28", &second_zone).await,
        ),
    ];

    for (index, (case, subnet)) in cases.into_iter().enumerate() {
        let file_system_id = efs.available_file_system(&format!("placement-{index}")).await;
        let created = efs
            .create_mount_target(json!({
                "FileSystemId": file_system_id,
                "SubnetId": subnet.subnet_id,
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            created.status, created.body
        );
        let mount_target_id = created.string("MountTargetId");
        assert!(
            matches_id_pattern(&mount_target_id, "fsmt-"),
            "case {case:?}: expected a minted fsmt- id, got {mount_target_id:?}"
        );
        assert_eq!(
            created.string("SubnetId"),
            subnet.subnet_id,
            "case {case:?}: the mount target is placed in the requested subnet, got {:?}",
            created.get("SubnetId")
        );
        assert_eq!(
            created.string("VpcId"),
            subnet.vpc_id,
            "case {case:?}: the VPC is the subnet's, as its owner reports it, got {:?}",
            created.get("VpcId")
        );
        assert_eq!(
            created.string("AvailabilityZoneName"),
            subnet.zone_name,
            "case {case:?}: the zone name is the subnet's, got {:?}",
            created.get("AvailabilityZoneName")
        );
        assert_eq!(
            created.string("AvailabilityZoneId"),
            subnet.availability_zone_id,
            "case {case:?}: the zone id is the subnet's, got {:?}",
            created.get("AvailabilityZoneId")
        );
        let address = created.string("IpAddress");
        assert!(
            subnet.holds(&address),
            "case {case:?}: the address must lie inside the subnet's own block {}, got \
             {address:?}",
            subnet.cidr_block
        );

        // Persisted, not projected once: the same placement comes back through the reader.
        let described = efs
            .describe_mount_targets(&format!("FileSystemId={file_system_id}"))
            .await;
        let element = &described.get("MountTargets")[0];
        assert_eq!(
            element["MountTargetId"].as_str(),
            Some(mount_target_id.as_str()),
            "case {case:?}: the described mount target is the created one, got {element:?}"
        );
        for member in [
            "SubnetId",
            "VpcId",
            "AvailabilityZoneName",
            "AvailabilityZoneId",
            "IpAddress",
            "FileSystemId",
            "OwnerId",
            "NetworkInterfaceId",
        ] {
            assert_eq!(
                &element[member],
                created.get(member),
                "case {case:?}: DescribeMountTargets must serve the created {member}, got {:?}",
                element[member]
            );
        }
    }
}

/// RULE: a `SubnetId` the account has no subnet for is refused with `SubnetNotFound` (400,
/// `ErrorCode` `SubnetNotFound`, "The subnet ID '<SubnetId>' does not exist") and nothing is
/// written — so the file system has no mount target and its delete is accepted.
#[tokio::test]
async fn a_subnet_the_account_does_not_have_is_refused_and_writes_nothing() {
    let efs = Emulator::new();
    let held = efs.account_subnet(0).await;
    let file_system_id = efs.available_file_system("unresolvable-subnet").await;

    // A real identifier with its last hex digit changed: the FORM is exactly one EC2 mints,
    // and only the resolution tells the two apart.
    let mut digits: Vec<char> = held.subnet_id.chars().collect();
    let last = digits.len() - 1;
    digits[last] = if digits[last] == '0' { '1' } else { '0' };
    let altered: String = digits.into_iter().collect();
    assert_ne!(
        altered, held.subnet_id,
        "the altered identifier must differ from the subnet the account has"
    );

    // A subnet its owner minted and then deleted: it stops resolving at once.
    let deleted = subnet_in_a_new_vpc(&efs, "10.96.0.0/16", "10.96.1.0/24", &held.zone_name).await;
    efs.ec2()
        .delete_subnet()
        .subnet_id(&deleted.subnet_id)
        .send()
        .await
        .expect("EC2 must delete the subnet it minted");

    let cases: Vec<(&str, String)> = vec![
        (
            "the narrowest identifier the model admits",
            NARROWEST_SUBNET_ID.to_string(),
        ),
        (
            "the widest identifier the model admits",
            WIDEST_SUBNET_ID.to_string(),
        ),
        ("a held identifier with one digit changed", altered),
        ("a subnet its owner has deleted", deleted.subnet_id.clone()),
    ];

    for (case, subnet_id) in cases {
        let refused = efs
            .create_mount_target(json!({
                "FileSystemId": file_system_id,
                "SubnetId": subnet_id,
            }))
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("SubnetNotFound"),
            "case {case:?}: expected the SubnetNotFound error type, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.string("ErrorCode"),
            "SubnetNotFound",
            "case {case:?}: expected the SubnetNotFound code, got {:?}",
            refused.get("ErrorCode")
        );
        assert_eq!(
            refused.string("Message"),
            format!("The subnet ID '{subnet_id}' does not exist"),
            "case {case:?}: the refusal names the requested subnet and nothing else, got {:?}",
            refused.get("Message")
        );
    }

    // Nothing was written for any of them.
    let described = efs
        .describe_mount_targets(&format!("FileSystemId={file_system_id}"))
        .await;
    assert_eq!(
        described.get("MountTargets"),
        &json!([]),
        "a refused CreateMountTarget must store no mount target, got {:?}",
        described.get("MountTargets")
    );
    let counted = efs
        .describe_file_systems(&format!("FileSystemId={file_system_id}"))
        .await;
    assert_eq!(
        counted.get("FileSystems")[0]["NumberOfMountTargets"],
        json!(0),
        "a refused CreateMountTarget must leave the count at 0, got {:?}",
        counted.get("FileSystems")[0]
    );

    // So the file system is not in use, and the teardown goes through: 204, empty body.
    let dropped = efs.delete_file_system(&file_system_id).await;
    assert_eq!(
        dropped.status, 204,
        "DeleteFileSystem after the refusals: expected 204, got {} {:?}",
        dropped.status, dropped.body
    );
    assert_eq!(
        dropped.body_len, 0,
        "DeleteFileSystem after the refusals: expected an empty body, got {} byte(s)",
        dropped.body_len
    );
    let gone = efs
        .describe_file_systems(&format!("FileSystemId={file_system_id}"))
        .await;
    assert_eq!(
        gone.status, 404,
        "DescribeFileSystems after the delete: expected 404, got {} {:?}",
        gone.status, gone.body
    );
}

/// RULE (the same one, on the subnet that IS held): a mount target in a held subnet makes the
/// file system in use, which its own refusal names — the 409 belongs to a mount target that
/// exists, not to one a refused create would have made.
#[tokio::test]
async fn only_a_mount_target_that_exists_makes_a_file_system_in_use() {
    let efs = Emulator::new();
    let subnet = efs.account_subnet(0).await;
    let file_system_id = efs.available_file_system("in-use-gate").await;

    let created = efs
        .create_mount_target(json!({
            "FileSystemId": file_system_id,
            "SubnetId": subnet.subnet_id,
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateMountTarget in a held subnet: expected 200, got {} {:?}",
        created.status, created.body
    );

    let in_use = efs.delete_file_system(&file_system_id).await;
    assert_eq!(
        (in_use.status, in_use.error_type.as_deref()),
        (409, Some("FileSystemInUse")),
        "DeleteFileSystem with a mount target: expected 409 FileSystemInUse, got {} {:?}",
        in_use.status,
        in_use.body
    );

    // Remove the one mount target that exists and the same delete is accepted.
    let removed = efs.delete_mount_target(&created.string("MountTargetId")).await;
    assert_eq!(
        removed.status, 204,
        "DeleteMountTarget: expected 204, got {} {:?}",
        removed.status, removed.body
    );
    let dropped = efs.delete_file_system(&file_system_id).await;
    assert_eq!(
        dropped.status, 204,
        "DeleteFileSystem after the mount target is gone: expected 204, got {} {:?}",
        dropped.status, dropped.body
    );
}
