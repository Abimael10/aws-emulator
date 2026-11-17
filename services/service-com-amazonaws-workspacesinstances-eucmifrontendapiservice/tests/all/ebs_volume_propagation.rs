// SPDX-License-Identifier: CC0-1.0

//! The volume `CreateVolume` reports EXISTS as an Amazon EBS volume of the account.
//!
//! RULE under test: a WorkSpaces Instances volume IS an EBS volume of the caller's own
//! account, so `CreateVolume` creates it in the service that OWNS EBS volumes, reports the
//! `VolumeId` that service minted, and `DeleteVolume` removes it there — and EBS's own
//! reads therefore resolve the reported id to a volume of the requested zone, size, type
//! and tags for as long as the volume exists, accept a tag on it, and resolve nothing once
//! it is deleted. A `SnapshotId` the account holds no snapshot for names nothing to restore,
//! so the create is refused with the backing service's own refusal.
//!
//! The rule holds in BOTH directions, because there is one volume: a volume removed through
//! the owning service's own API is gone, so no operation of this service still acts on it —
//! it answers the not-found it answers for an id that was never minted, and the `ClientToken`
//! of the create that made it is free again, exactly as this service's own `DeleteVolume`
//! leaves it.
//!
//! Nothing here reaches into either store: the volume is believed to exist only because
//! `ec2:DescribeVolumes` reported it, and its configuration only because EC2 reported that
//! too. EC2 is driven through the AWS SDK over the router's own in-process callback.
//!
//! The rows span what the rule generalises over: the volume type (each of the seven the
//! model admits), the size (the published bottom of each type's range and sizes above it),
//! the performance members a type takes (`Iops`, `Throughput`), the encryption state and
//! key, the zone (letters inside and past the first three a Region publishes), and where
//! the tags come from (none, one specification, two).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_instance_propagation::ec2_client;
use crate::workspace_instance_lifecycle::{call, error_code, ok, router};

/// How long the emulator is given to bring a volume to a state its own clock owes it — a
/// create to `available`. Nothing here sleeps for a span: the test looks again until the
/// condition holds, with generous room so a loaded host cannot fail it.
const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// How long this suite waits between looks while polling for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `VolumeState` values this suite names
/// (`smithy_query(query="shape",shape="VolumeState")`).
const STATE_CREATING: &str = "creating";
const STATE_AVAILABLE: &str = "available";

/// One create the rule is asserted over.
struct Row {
    case: &'static str,
    /// The `CreateVolume` body.
    body: Value,
    /// The zone the EBS volume must report.
    zone: &'static str,
    /// The size in GiB it must report.
    size: i32,
    /// The `VolumeType` it must report — the request's own, or the backing service's
    /// documented default where the request names none.
    volume_type: &'static str,
    /// The IOPS and throughput it must report, where the type provisions them.
    iops: Option<i32>,
    throughput: Option<i32>,
    /// Whether it must report itself encrypted.
    encrypted: bool,
    /// The tags it must carry.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows: one per thing the rule generalises over.
///
/// Every size is inside the published range of its type and every `Iops`/`Throughput`
/// inside the published range of the member for that type (`crate::state::VOLUME_TYPE_SPECS`,
/// whose doc quotes the Amazon EBS `CreateVolume` reference). `us-east-1d` is a zone the
/// Region publishes past the first three letters, so the rule is shown not to turn on the
/// zone letter.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the smallest volume the default type takes",
            body: json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 1 }),
            zone: "us-east-1a",
            size: 1,
            // "Default: gp2" (the Amazon EBS `CreateVolume$VolumeType`).
            volume_type: "gp2",
            iops: None,
            throughput: None,
            encrypted: false,
            tags: &[],
        },
        Row {
            case: "a gp3 volume with the IOPS and throughput it provisions",
            body: json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 20,
                "VolumeType": "gp3",
                "Iops": 4_000,
                "Throughput": 250,
            }),
            zone: "us-east-1b",
            size: 20,
            volume_type: "gp3",
            iops: Some(4_000),
            throughput: Some(250),
            encrypted: false,
            tags: &[],
        },
        Row {
            case: "a gp3 volume that names neither IOPS nor throughput",
            body: json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 100,
                "VolumeType": "gp3",
            }),
            zone: "us-east-1c",
            size: 100,
            volume_type: "gp3",
            // "gp3: 3,000 (default)" and the bottom of the published gp3 throughput range.
            iops: Some(3_000),
            throughput: Some(125),
            encrypted: false,
            tags: &[],
        },
        Row {
            case: "an io1 volume, whose type requires provisioned IOPS",
            body: json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 4,
                "VolumeType": "io1",
                "Iops": 100,
            }),
            zone: "us-east-1a",
            size: 4,
            volume_type: "io1",
            iops: Some(100),
            throughput: None,
            encrypted: false,
            tags: &[],
        },
        Row {
            case: "an io2 volume with one tag specification",
            body: json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 500,
                "VolumeType": "io2",
                "Iops": 1_000,
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "env", "Value": "dev" }] },
                ],
            }),
            zone: "us-east-1b",
            size: 500,
            volume_type: "io2",
            iops: Some(1_000),
            throughput: None,
            encrypted: false,
            tags: &[("env", "dev")],
        },
        Row {
            case: "an st1 volume at the bottom of its published size range, with two tags",
            body: json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 125,
                "VolumeType": "st1",
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "team", "Value": "eu" }] },
                    { "Tags": [{ "Key": "owner", "Value": "alice" }] },
                ],
            }),
            zone: "us-east-1c",
            size: 125,
            volume_type: "st1",
            iops: None,
            throughput: None,
            encrypted: false,
            tags: &[("owner", "alice"), ("team", "eu")],
        },
        Row {
            case: "an sc1 volume in a zone past the Region's first three letters",
            body: json!({
                "AvailabilityZone": "us-east-1d",
                "SizeInGB": 1_000,
                "VolumeType": "sc1",
            }),
            zone: "us-east-1d",
            size: 1_000,
            volume_type: "sc1",
            iops: None,
            throughput: None,
            encrypted: false,
            tags: &[],
        },
        Row {
            case: "an encrypted standard volume under a named KMS key",
            body: json!({
                "AvailabilityZone": "us-east-1f",
                "SizeInGB": 1_024,
                "VolumeType": "standard",
                "Encrypted": true,
                "KmsKeyId": "alias/aws/ebs",
            }),
            zone: "us-east-1f",
            size: 1_024,
            volume_type: "standard",
            iops: None,
            throughput: None,
            encrypted: true,
            tags: &[],
        },
    ]
}

/// `CreateVolume` with `body`, asserting a 200 and returning the minted id.
async fn create_volume(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let payload = ok(router, "CreateVolume", body).await;
    payload["VolumeId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume returned no VolumeId: {payload}"))
        .to_string()
}

/// The volume Amazon EC2 holds for `volume_id`, or `None` when it holds none.
async fn described(
    ec2: &::aws_sdk_ec2::Client,
    volume_id: &str,
) -> Option<::aws_sdk_ec2::types::Volume> {
    let answered = ec2
        .describe_volumes()
        .volume_ids(volume_id)
        .send()
        .await
        .ok()?;
    answered
        .volumes()
        .iter()
        .find(|volume| volume.volume_id() == Some(volume_id))
        .cloned()
}

/// Look again until Amazon EC2 reports `volume_id` in one of `states`, and answer the
/// volume it reported. Waits for the CONDITION, not for a span: the deadline is only the
/// point past which the condition is taken to be unreachable.
async fn settled_in(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    volume_id: &str,
    states: &[&str],
) -> ::aws_sdk_ec2::types::Volume {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let reported = described(ec2, volume_id).await;
        if let Some(volume) = reported.as_ref() {
            let state = volume.state().map(|state| state.as_str().to_string());
            if state
                .as_deref()
                .is_some_and(|state| states.contains(&state))
            {
                return volume.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected EC2 to report {volume_id} in one of {states:?}, it \
             reports {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The tags EC2 reports on a volume, as a map.
fn volume_tags(volume: &::aws_sdk_ec2::types::Volume) -> BTreeMap<String, String> {
    volume
        .tags()
        .iter()
        .filter_map(|tag| {
            Some((
                tag.key()?.to_string(),
                tag.value().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

#[tokio::test]
async fn every_created_volume_is_an_ebs_volume_of_the_account() {
    let router = router();
    let ec2 = ec2_client(&router);

    let mut reported_ids: Vec<String> = Vec::new();
    for row in rows() {
        let case = row.case;
        let volume_id = create_volume(&router, case, row.body.clone()).await;
        assert!(
            volume_id.starts_with("vol-"),
            "case {case:?}: expected an EBS volume id, got {volume_id:?}"
        );
        assert!(
            !reported_ids.contains(&volume_id),
            "case {case:?}: expected a volume of its own, got the already-reported \
             {volume_id:?}"
        );

        // The service that OWNS EBS volumes holds it: this is the whole point of
        // reporting the id. A volume is answered `creating` and reaches `available`
        // without anyone asking.
        let volume = settled_in(&ec2, case, &volume_id, &[STATE_CREATING, STATE_AVAILABLE]).await;
        assert_eq!(
            volume.availability_zone(),
            Some(row.zone),
            "case {case:?}: expected the volume in the requested zone, got {volume:?}"
        );
        assert_eq!(
            volume.size(),
            Some(row.size),
            "case {case:?}: expected the requested size, got {volume:?}"
        );
        assert_eq!(
            volume.volume_type().map(|kind| kind.as_str()),
            Some(row.volume_type),
            "case {case:?}: expected the requested volume type, got {volume:?}"
        );
        assert_eq!(
            volume.iops(),
            row.iops,
            "case {case:?}: expected the IOPS the type provisions, got {volume:?}"
        );
        assert_eq!(
            volume.throughput(),
            row.throughput,
            "case {case:?}: expected the throughput the type provisions, got {volume:?}"
        );
        assert_eq!(
            volume.encrypted(),
            Some(row.encrypted),
            "case {case:?}: expected the requested encryption state, got {volume:?}"
        );
        let carried = volume_tags(&volume);
        let expected: BTreeMap<String, String> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            carried, expected,
            "case {case:?}: expected the requested tags, got {carried:?}"
        );

        // And EC2's own clock brings it to `available`, which is what makes it a volume
        // of the account rather than a record.
        settled_in(&ec2, case, &volume_id, &[STATE_AVAILABLE]).await;

        // A volume of the account is one EC2 will TAG: the id resolves on the write path
        // too, and the tag it adds is on the volume this service created.
        ec2.create_tags()
            .resources(&volume_id)
            .tags(
                ::aws_sdk_ec2::types::Tag::builder()
                    .key("added-by-ec2")
                    .value("1")
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: ec2:CreateTags on {volume_id} was refused: {error}")
            });
        let tagged = described(&ec2, &volume_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: EC2 resolves no volume {volume_id:?}"));
        assert_eq!(
            volume_tags(&tagged).get("added-by-ec2").map(String::as_str),
            Some("1"),
            "case {case:?}: expected the tag EC2 added, got {tagged:?}"
        );

        reported_ids.push(volume_id);
    }

    // Every volume created above is still one EC2 holds, so no row displaced another.
    for volume_id in &reported_ids {
        assert!(
            described(&ec2, volume_id).await.is_some(),
            "case the whole table: expected EC2 to still hold {volume_id}"
        );
    }
}

#[tokio::test]
async fn deleting_a_volume_removes_the_ebs_volume() {
    let router = router();
    let ec2 = ec2_client(&router);

    // Rows differing in how the volume was configured, so the removal is shown for a
    // plain volume, a tagged one and one of a provisioned-IOPS type.
    let configurations = [
        (
            "a volume of the default type",
            json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 8 }),
        ),
        (
            "a tagged gp3 volume",
            json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 30,
                "VolumeType": "gp3",
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "k", "Value": "v" }] },
                ],
            }),
        ),
        (
            "an io2 volume",
            json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 4,
                "VolumeType": "io2",
                "Iops": 200,
            }),
        ),
    ];

    for (case, body) in configurations {
        let volume_id = create_volume(&router, case, body).await;
        settled_in(&ec2, case, &volume_id, &[STATE_CREATING, STATE_AVAILABLE]).await;

        ok(&router, "DeleteVolume", json!({ "VolumeId": &volume_id })).await;

        // The volume is gone from the account: EBS's own read resolves nothing, which is
        // what makes the delete a delete rather than a forgotten record.
        assert!(
            described(&ec2, &volume_id).await.is_none(),
            "case {case:?}: expected EC2 to hold no volume {volume_id} after the delete"
        );
        let refused = ec2
            .describe_volumes()
            .volume_ids(&volume_id)
            .send()
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("case {case:?}: ec2:DescribeVolumes must refuse the deleted {volume_id}")
            });
        assert_eq!(
            ::aws_sdk_ec2::error::ProvideErrorMetadata::code(&refused),
            Some("InvalidVolume.NotFound"),
            "case {case:?}: expected EC2's own code for a volume it holds none of, got \
             {refused:?}"
        );

        // And this service answers its own not-found for the same id.
        let (status, payload) = call(&router, "DeleteVolume", json!({ "VolumeId": &volume_id })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a second delete to be refused, got {status} with {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ResourceNotFoundException",
            "case {case:?}: expected this service's not-found, got {payload}"
        );
    }
}

#[tokio::test]
async fn a_snapshot_the_account_does_not_hold_is_refused_with_the_backing_services_refusal() {
    let router = router();
    let ec2 = ec2_client(&router);

    // Rows: snapshot ids inside the modelled `^snap-[0-9a-zA-Z]{1,63}$`
    // (`smithy_query(query="shape",shape="SnapshotId")`) that name no snapshot — the
    // shortest the pattern admits, the 8- and 17-hex-digit forms EC2 issues, a mixed-case
    // one, and the longest the pattern admits — each paired with a request that names a
    // size and one that leaves the size to the snapshot.
    let snapshots = [
        ("the shortest id the pattern admits", "snap-a".to_string()),
        ("the short id form", "snap-0abc1234".to_string()),
        ("the long id form", "snap-0123456789abcdef0".to_string()),
        ("a mixed-case id", "snap-00000000DeadBeef".to_string()),
        (
            "the longest id the pattern admits",
            format!("snap-{}", "0".repeat(63)),
        ),
    ];

    for (case, snapshot_id) in snapshots {
        for (shape, body) in [
            (
                "with no size of its own",
                json!({ "AvailabilityZone": "us-east-1a", "SnapshotId": &snapshot_id }),
            ),
            (
                "with a size beside it",
                json!({
                    "AvailabilityZone": "us-east-1b",
                    "SizeInGB": 10,
                    "VolumeType": "gp3",
                    "SnapshotId": &snapshot_id,
                }),
            ),
        ] {
            let case = format!("{case} {shape}");
            let (status, payload) = call(&router, "CreateVolume", body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a SnapshotId the account holds no snapshot for must be \
                 refused, got {status} with {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {payload}"
            );
            // The sentence is the backing service's, carried whole: the fixed text with
            // the snapshot id the request named inside it.
            let expected = format!(
                "Create volume failed (InvalidSnapshotID.Malformed: Value ( {snapshot_id} ) \
                 for parameter SNAPSHOT is invalid. )"
            );
            assert_eq!(
                payload["Message"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {payload}"
            );
        }
    }

    // Nothing was created for any of those refusals: the account holds no volume at all.
    let held = ec2
        .describe_volumes()
        .send()
        .await
        .expect("ec2:DescribeVolumes answers");
    assert!(
        held.volumes().is_empty(),
        "case the whole table: a refused create must create no volume, EC2 holds {:?}",
        held.volumes()
    );
}

#[tokio::test]
async fn a_volume_restored_from_a_snapshot_the_account_holds_is_created_from_it() {
    let router = router();
    let ec2 = ec2_client(&router);

    // Rows differing in the source volume's size, so the restored volume cannot pass by
    // reporting one fixed size.
    for (case, source_size) in [("a small source volume", 8), ("a larger source volume", 40)] {
        let source = create_volume(
            &router,
            case,
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": source_size,
                "VolumeType": "gp3",
            }),
        )
        .await;
        // A snapshot is taken of a volume the account holds, which is what the create
        // above left behind; the snapshot is EC2's own and so is its id.
        settled_in(&ec2, case, &source, &[STATE_AVAILABLE]).await;
        let snapshot = ec2
            .create_snapshot()
            .volume_id(&source)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: ec2:CreateSnapshot of {source} was refused: {error}")
            });
        let snapshot_id = snapshot
            .snapshot_id()
            .unwrap_or_else(|| panic!("case {case:?}: ec2:CreateSnapshot minted no id"))
            .to_string();

        // The create names the snapshot and no size at all: "a volume with no size of its
        // own takes the snapshot's".
        let restored = create_volume(
            &router,
            case,
            json!({ "AvailabilityZone": "us-east-1a", "SnapshotId": &snapshot_id }),
        )
        .await;
        let volume = settled_in(
            &ec2,
            case,
            &restored,
            &[STATE_CREATING, STATE_AVAILABLE],
        )
        .await;
        assert_eq!(
            volume.snapshot_id(),
            Some(snapshot_id.as_str()),
            "case {case:?}: expected the restored volume to name its snapshot, got {volume:?}"
        );
        assert_eq!(
            volume.size(),
            Some(source_size),
            "case {case:?}: expected the snapshot's own size, got {volume:?}"
        );
    }
}

#[tokio::test]
async fn a_volume_the_owning_service_removed_resolves_nowhere_here_either() {
    let router = router();
    let ec2 = ec2_client(&router);

    // Rows differing in the configuration of the volume the account loses, and in the size
    // a modification of it would ask for — so the refusals cannot turn on one volume's
    // shape.
    let configurations = [
        (
            "a volume of the default type",
            json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 8 }),
            16,
        ),
        (
            "a tagged gp3 volume",
            json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 30,
                "VolumeType": "gp3",
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "k", "Value": "v" }] },
                ],
            }),
            60,
        ),
        (
            "an io2 volume",
            json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 4,
                "VolumeType": "io2",
                "Iops": 200,
            }),
            40,
        ),
    ];

    for (case, body, grown) in configurations {
        let volume_id = create_volume(&router, case, body).await;
        settled_in(&ec2, case, &volume_id, &[STATE_AVAILABLE]).await;

        // The volume is removed through the service that OWNS it, which is how these
        // volumes are managed: this API exposes no describe or tag of its own.
        ec2.delete_volume()
            .volume_id(&volume_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: ec2:DeleteVolume of {volume_id} was refused: {error}")
            });
        assert!(
            described(&ec2, &volume_id).await.is_none(),
            "case {case:?}: expected EC2 to hold no volume {volume_id} after its own delete"
        );

        // So the account holds no such volume at all, and every operation of this service
        // that acts on one answers as it does for an id that was never minted: there is ONE
        // volume, and the two services cannot disagree about whether it exists.
        for (operation, request) in [
            ("DeleteVolume", json!({ "VolumeId": &volume_id })),
            (
                "ModifyVolume",
                json!({ "VolumeId": &volume_id, "SizeInGB": grown }),
            ),
        ] {
            let (status, payload) = call(&router, operation, request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected {operation} on a volume the account no longer holds \
                 to be refused, got {status} with {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "ResourceNotFoundException",
                "case {case:?}: expected {operation} to answer this service's not-found, got \
                 {payload}"
            );
        }
    }
}

#[tokio::test]
async fn the_client_token_of_a_volume_the_account_lost_is_free_again() {
    let router = router();
    let ec2 = ec2_client(&router);

    // Rows differing in the token's own form, inside the modelled `ClientToken`
    // (len 1..64, ^[\x20-\x7E]+$ — `smithy_query(query="shape",shape="ClientToken")`): the
    // shortest the shape admits, an ordinary one, and the longest.
    let tokens = [
        ("the shortest token the shape admits", "t".to_string()),
        ("an ordinary token", "volume-token-1".to_string()),
        ("the longest token the shape admits", "x".repeat(64)),
    ];

    for (case, token) in tokens {
        let request = json!({
            "AvailabilityZone": "us-east-1a",
            "SizeInGB": 12,
            "VolumeType": "gp3",
            "ClientToken": &token,
        });
        let first = create_volume(&router, case, request.clone()).await;
        settled_in(&ec2, case, &first, &[STATE_AVAILABLE]).await;

        // While the volume that token created exists, the token is spent.
        let (status, payload) = call(&router, "CreateVolume", request.clone()).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected a spent token to be refused while its volume exists, got \
             {status} with {payload}"
        );

        // Once the owning service has removed the volume, the record that token created is
        // gone with it — so the token can create a volume again, exactly as it can after
        // this service's own DeleteVolume.
        ec2.delete_volume()
            .volume_id(&first)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: ec2:DeleteVolume of {first} was refused: {error}")
            });
        let second = create_volume(&router, case, request).await;
        assert_ne!(
            second, first,
            "case {case:?}: expected a volume of its own, got the removed {first}"
        );
        settled_in(&ec2, case, &second, &[STATE_CREATING, STATE_AVAILABLE]).await;
    }
}
