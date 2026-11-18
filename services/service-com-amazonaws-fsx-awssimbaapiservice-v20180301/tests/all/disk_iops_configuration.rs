// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the provisioned SSD IOPS of an FSx file system, driven over the
//! wire.
//!
//! RULE: `DiskIopsConfiguration` is a server-side default of every file-system
//! configuration that models it, not an echo of the request — AWS documents that "by
//! default, Amazon FSx automatically provisions 3 IOPS per GB of storage capacity", and
//! `DiskIopsConfiguration$Iops` states the same rate as `StorageCapacity * 3 * HAPairs` —
//! so a created file system reports `Mode` `AUTOMATIC` with that rate applied to its own
//! stored storage capacity and HA pairs unless the caller provisioned a
//! `USER_PROVISIONED` value, which is reported back unchanged, and every read of the
//! record (including the copy a backup keeps of the file system it was taken from) reports
//! the same member.
//!
//! Each case asserts that rule over a SET of inputs — every configuration that models the
//! member, a span of storage capacities, a span of HA pairs, both modes, and a capacity
//! that changes after creation — rather than over one recorded call. The requests go
//! through the same front door as `file_system_lifecycle.rs` (its harness is reused), so
//! nothing here reaches into the store directly.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::backup_lifecycle::describe_one as describe_backup;
use crate::file_system_lifecycle::{
    call, emulator, ok, with_client_request_token, PAST_TRANSITION,
};

/// The modelled `DiskIopsConfigurationMode` values
/// (`smithy_query(query="shape", shape="DiskIopsConfigurationMode")`: AUTOMATIC |
/// USER_PROVISIONED).
const MODE_AUTOMATIC: &str = "AUTOMATIC";
/// See [`MODE_AUTOMATIC`].
const MODE_USER_PROVISIONED: &str = "USER_PROVISIONED";

/// The bounds of the modelled `DiskIopsConfiguration$Iops` long (`range 0..2147483647`).
const IOPS_RANGE_MIN: i64 = 0;
/// See [`IOPS_RANGE_MIN`].
const IOPS_RANGE_MAX: i64 = 2_147_483_647;

/// One row of the table: a file system of one type, at one storage capacity, and the SSD
/// IOPS its configuration must report once created.
struct Row {
    /// The modelled `FileSystemType` value.
    file_system_type: &'static str,
    /// The `FileSystem` member the per-type configuration is reported in.
    member: &'static str,
    /// The create request's per-type configuration, carrying the members the modelled
    /// shape requires and NO `DiskIopsConfiguration` of its own.
    configuration: Value,
    /// `StorageCapacity`, in GiB.
    storage_capacity: i64,
    /// The `Iops` the file system must report: 3 per GiB of `storage_capacity`, times the
    /// configuration's HA pairs. Written out per row so this test never re-derives the
    /// value the service computes.
    expected_iops: i64,
}

/// The table every `AUTOMATIC` case below runs: the three configurations that model
/// `DiskIopsConfiguration` (`WindowsFileSystemConfiguration`,
/// `OntapFileSystemConfiguration`, `OpenZFSFileSystemConfiguration`), spanning storage
/// capacities and — for ONTAP, the one configuration that models `HAPairs` — the absent,
/// the default and multi-pair counts the member documents (1..12, "The default value is
/// 1"; more than one pair needs the `SINGLE_AZ_2` deployment type).
fn automatic_rows() -> Vec<Row> {
    vec![
        Row {
            file_system_type: "OPENZFS",
            member: "OpenZFSConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 64 }),
            storage_capacity: 64,
            // 3 * 64
            expected_iops: 192,
        },
        Row {
            file_system_type: "OPENZFS",
            member: "OpenZFSConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_2", "ThroughputCapacity": 160 }),
            storage_capacity: 1200,
            // 3 * 1200
            expected_iops: 3600,
        },
        Row {
            file_system_type: "WINDOWS",
            member: "WindowsConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 32 }),
            storage_capacity: 32,
            // 3 * 32
            expected_iops: 96,
        },
        Row {
            file_system_type: "WINDOWS",
            member: "WindowsConfiguration",
            configuration: json!({ "DeploymentType": "MULTI_AZ_1", "ThroughputCapacity": 128 }),
            storage_capacity: 2000,
            // 3 * 2000
            expected_iops: 6000,
        },
        Row {
            file_system_type: "ONTAP",
            member: "OntapConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 128 }),
            storage_capacity: 1024,
            // 3 * 1024 * the default 1 HA pair
            expected_iops: 3072,
        },
        Row {
            file_system_type: "ONTAP",
            member: "OntapConfiguration",
            configuration: json!({
                "DeploymentType": "SINGLE_AZ_1",
                "ThroughputCapacity": 128,
                "HAPairs": 1,
            }),
            storage_capacity: 1024,
            // 3 * 1024 * 1
            expected_iops: 3072,
        },
        Row {
            file_system_type: "ONTAP",
            member: "OntapConfiguration",
            configuration: json!({
                "DeploymentType": "SINGLE_AZ_2",
                "ThroughputCapacityPerHAPair": 1536,
                "HAPairs": 2,
            }),
            storage_capacity: 2048,
            // 3 * 2048 * 2
            expected_iops: 12_288,
        },
        Row {
            file_system_type: "ONTAP",
            member: "OntapConfiguration",
            configuration: json!({
                "DeploymentType": "SINGLE_AZ_2",
                "ThroughputCapacityPerHAPair": 1536,
                "HAPairs": 12,
            }),
            storage_capacity: 12_288,
            // 3 * 12288 * 12
            expected_iops: 442_368,
        },
    ]
}

/// A `CreateFileSystem` request for one per-type configuration, carrying the required
/// `ClientRequestToken`.
fn create_request(
    file_system_type: &str,
    member: &str,
    configuration: &Value,
    storage_capacity: i64,
    subnets: &[&str],
) -> Value {
    let mut request = json!({
        "FileSystemType": file_system_type,
        "StorageCapacity": storage_capacity,
        "StorageType": "SSD",
        "SubnetIds": subnets,
    });
    request[member] = configuration.clone();
    with_client_request_token(&request)
}

/// The `DiskIopsConfiguration` a `FileSystem`'s per-type configuration reports, as
/// `(Mode, Iops)`, or `None` when the member is absent.
fn disk_iops(file_system: &Value, member: &str) -> Option<(String, Option<i64>)> {
    let configuration = file_system[member].get("DiskIopsConfiguration")?;
    Some((
        configuration["Mode"].as_str().unwrap_or_default().to_string(),
        configuration["Iops"].as_i64(),
    ))
}

/// Describe exactly one file system, requiring it to exist.
async fn describe_one(router: &MultiServiceRouter, id: &str) -> Value {
    let body = ok(
        router,
        "DescribeFileSystems",
        &json!({ "FileSystemIds": [id] }),
    )
    .await;
    let systems = body["FileSystems"]
        .as_array()
        .expect("FileSystems must be a list")
        .clone();
    assert_eq!(
        systems.len(),
        1,
        "describing {id} must return exactly one file system, got {body}"
    );
    systems[0].clone()
}

/// The `FileSystem` a backup of `file_system_id` recorded, read back through
/// `DescribeBackups` so it is the stored copy and not the create response.
async fn backed_up_file_system(router: &MultiServiceRouter, file_system_id: &str) -> Value {
    let created = ok(
        router,
        "CreateBackup",
        &with_client_request_token(&json!({ "FileSystemId": file_system_id })),
    )
    .await;
    let backup_id = created["Backup"]["BackupId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !backup_id.is_empty(),
        "CreateBackup must answer with a BackupId, got {created}"
    );
    let body = ok(router, "DescribeBackups", &json!({ "BackupIds": [backup_id] })).await;
    body["Backups"][0]["FileSystem"].clone()
}

/// RULE: a file system created with no `DiskIopsConfiguration` still HAS one — `AUTOMATIC`
/// with 3 IOPS per GiB of its own storage capacity, times its HA pairs — on the create
/// response, on every `DescribeFileSystems` read, and on the file system a backup recorded.
#[tokio::test]
async fn an_omitted_disk_iops_configuration_is_defaulted_to_automatic_three_iops_per_gib() {
    let router = emulator().await;
    for row in automatic_rows() {
        let case = format!(
            "{} {} GiB {}",
            row.file_system_type, row.storage_capacity, row.configuration
        );
        let subnets = [router.subnet_a(), router.subnet_b()];
        let created = ok(
            &router,
            "CreateFileSystem",
            &create_request(
                row.file_system_type,
                row.member,
                &row.configuration,
                row.storage_capacity,
                &subnets,
            ),
        )
        .await;
        let file_system = created["FileSystem"].clone();
        let file_system_id = file_system["FileSystemId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let expected = Some((MODE_AUTOMATIC.to_string(), Some(row.expected_iops)));

        assert_eq!(
            disk_iops(&file_system, row.member),
            expected,
            "case {case:?}: expected the created file system to report Mode {MODE_AUTOMATIC} \
             with Iops {}, got {:?}",
            row.expected_iops,
            file_system[row.member]
        );

        tokio::time::sleep(PAST_TRANSITION).await;
        let described = describe_one(&router, &file_system_id).await;
        assert_eq!(
            disk_iops(&described, row.member),
            expected,
            "case {case:?}: expected DescribeFileSystems to report Mode {MODE_AUTOMATIC} with \
             Iops {}, got {:?}",
            row.expected_iops,
            described[row.member]
        );

        let recorded = backed_up_file_system(&router, &file_system_id).await;
        assert_eq!(
            disk_iops(&recorded, row.member),
            expected,
            "case {case:?}: expected the backup's recorded file system to report Mode \
             {MODE_AUTOMATIC} with Iops {}, got {:?}",
            row.expected_iops,
            recorded[row.member]
        );
    }
}

/// RULE: a `USER_PROVISIONED` configuration is the caller's to provision, so the mode and
/// the `Iops` it states are reported back unchanged — at both bounds of the modelled
/// `Iops` range and at values the automatic rate would never produce — while a
/// configuration that states `AUTOMATIC` and no `Iops` gets the computed rate.
#[tokio::test]
async fn a_provisioned_disk_iops_configuration_is_reported_back_unchanged() {
    let router = emulator().await;
    // (the requested configuration, the Mode and Iops it must report back). The storage
    // capacity is the same 64 GiB in every row, whose automatic rate is 192, so a row that
    // reports its own value cannot be the default in disguise.
    let storage_capacity = 64;
    let cases: &[(Value, &str, Option<i64>)] = &[
        (
            json!({ "Mode": MODE_USER_PROVISIONED, "Iops": IOPS_RANGE_MIN }),
            MODE_USER_PROVISIONED,
            Some(IOPS_RANGE_MIN),
        ),
        (
            json!({ "Mode": MODE_USER_PROVISIONED, "Iops": 400 }),
            MODE_USER_PROVISIONED,
            Some(400),
        ),
        (
            json!({ "Mode": MODE_USER_PROVISIONED, "Iops": 160_000 }),
            MODE_USER_PROVISIONED,
            Some(160_000),
        ),
        (
            json!({ "Mode": MODE_USER_PROVISIONED, "Iops": IOPS_RANGE_MAX }),
            MODE_USER_PROVISIONED,
            Some(IOPS_RANGE_MAX),
        ),
        // `AUTOMATIC` named explicitly is the same mode an omitted member gets, so it
        // carries the computed rate: 3 * 64.
        (
            json!({ "Mode": MODE_AUTOMATIC }),
            MODE_AUTOMATIC,
            Some(192),
        ),
    ];

    for (requested, expected_mode, expected_iops) in cases {
        for (file_system_type, member, mut configuration) in [
            (
                "OPENZFS",
                "OpenZFSConfiguration",
                json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 64 }),
            ),
            (
                "WINDOWS",
                "WindowsConfiguration",
                json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 32 }),
            ),
            (
                "ONTAP",
                "OntapConfiguration",
                json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 128 }),
            ),
        ] {
            let case = format!("{file_system_type} with DiskIopsConfiguration {requested}");
            configuration["DiskIopsConfiguration"] = requested.clone();
            let subnets = [router.subnet_a(), router.subnet_b()];
            let created = ok(
                &router,
                "CreateFileSystem",
                &create_request(
                    file_system_type,
                    member,
                    &configuration,
                    storage_capacity,
                    &subnets,
                ),
            )
            .await;
            let file_system_id = created["FileSystem"]["FileSystemId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let expected = Some(((*expected_mode).to_string(), *expected_iops));

            assert_eq!(
                disk_iops(&created["FileSystem"], member),
                expected,
                "case {case:?}: expected the created file system to report {expected:?}, got \
                 {:?}",
                created["FileSystem"][member]
            );

            tokio::time::sleep(PAST_TRANSITION).await;
            let described = describe_one(&router, &file_system_id).await;
            assert_eq!(
                disk_iops(&described, member),
                expected,
                "case {case:?}: expected DescribeFileSystems to report {expected:?}, got {:?}",
                described[member]
            );
        }
    }
}

/// RULE: `Iops` past the modelled `range 0..2147483647` is not a value the member can
/// carry, so the create is refused rather than stored.
#[tokio::test]
async fn disk_iops_past_the_modelled_range_is_refused() {
    let router = emulator().await;
    for iops in [IOPS_RANGE_MIN - 1, IOPS_RANGE_MAX + 1] {
        let subnets = [router.subnet_a()];
        let body = create_request(
            "OPENZFS",
            "OpenZFSConfiguration",
            &json!({
                "DeploymentType": "SINGLE_AZ_1",
                "ThroughputCapacity": 64,
                "DiskIopsConfiguration": { "Mode": MODE_USER_PROVISIONED, "Iops": iops },
            }),
            64,
            &subnets,
        );
        let (status, parsed) = call(&router, "CreateFileSystem", &body).await;
        assert_eq!(
            status, 400,
            "case {iops:?}: expected HTTP 400 for an Iops outside the modelled range, got \
             {status} with body {parsed}"
        );
    }
}

/// RULE: the `AUTOMATIC` count is derived from the file system's CURRENT capacity and HA
/// pairs, so an update that changes either moves it, while a `USER_PROVISIONED` value
/// stays exactly as provisioned until the caller provisions another.
#[tokio::test]
async fn automatic_disk_iops_follows_the_file_systems_capacity() {
    let router = emulator().await;

    // An OpenZFS file system left on the default: growing its capacity grows its IOPS at
    // the same 3-per-GiB rate.
    let subnets = [router.subnet_a()];
    let created = ok(
        &router,
        "CreateFileSystem",
        &create_request(
            "OPENZFS",
            "OpenZFSConfiguration",
            &json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 64 }),
            64,
            &subnets,
        ),
    )
    .await;
    let automatic_id = created["FileSystem"]["FileSystemId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    tokio::time::sleep(PAST_TRANSITION).await;
    let updated = ok(
        &router,
        "UpdateFileSystem",
        &json!({ "FileSystemId": automatic_id, "StorageCapacity": 256 }),
    )
    .await;
    assert_eq!(
        disk_iops(&updated["FileSystem"], "OpenZFSConfiguration"),
        // 3 * 64: the capacity the file system STILL has. Amazon FSx does not apply a
        // capacity target in line — it records a FILE_SYSTEM_UPDATE administrative action —
        // so the derived IOPS move only when that action completes.
        Some((MODE_AUTOMATIC.to_string(), Some(192))),
        "case \"OPENZFS asked to grow to 256 GiB\": expected Mode {MODE_AUTOMATIC} with the \
         current Iops 192, got {:?}",
        updated["FileSystem"]["OpenZFSConfiguration"]
    );
    tokio::time::sleep(PAST_TRANSITION).await;
    let described = describe_one(&router, &automatic_id).await;
    assert_eq!(
        disk_iops(&described, "OpenZFSConfiguration"),
        Some((MODE_AUTOMATIC.to_string(), Some(768))),
        "case \"OPENZFS grown to 256 GiB\": expected DescribeFileSystems to report Iops 768, \
         got {:?}",
        described["OpenZFSConfiguration"]
    );

    // A Windows file system the caller provisioned: the same growth leaves its IOPS alone,
    // and a later update that provisions another value replaces it.
    let created = ok(
        &router,
        "CreateFileSystem",
        &create_request(
            "WINDOWS",
            "WindowsConfiguration",
            &json!({
                "DeploymentType": "SINGLE_AZ_1",
                "ThroughputCapacity": 32,
                "DiskIopsConfiguration": { "Mode": MODE_USER_PROVISIONED, "Iops": 400 },
            }),
            32,
            &subnets,
        ),
    )
    .await;
    let provisioned_id = created["FileSystem"]["FileSystemId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    tokio::time::sleep(PAST_TRANSITION).await;
    let updated = ok(
        &router,
        "UpdateFileSystem",
        &json!({ "FileSystemId": provisioned_id, "StorageCapacity": 2000 }),
    )
    .await;
    assert_eq!(
        disk_iops(&updated["FileSystem"], "WindowsConfiguration"),
        Some((MODE_USER_PROVISIONED.to_string(), Some(400))),
        "case \"WINDOWS provisioned 400, grown to 2000 GiB\": expected the provisioned Iops \
         400 to stand, got {:?}",
        updated["FileSystem"]["WindowsConfiguration"]
    );
    tokio::time::sleep(PAST_TRANSITION).await;
    let updated = ok(
        &router,
        "UpdateFileSystem",
        &json!({
            "FileSystemId": provisioned_id,
            "WindowsConfiguration": {
                "DiskIopsConfiguration": { "Mode": MODE_USER_PROVISIONED, "Iops": 1000 },
            },
        }),
    )
    .await;
    assert_eq!(
        disk_iops(&updated["FileSystem"], "WindowsConfiguration"),
        Some((MODE_USER_PROVISIONED.to_string(), Some(1000))),
        "case \"WINDOWS re-provisioned to 1000\": expected the newly provisioned Iops, got {:?}",
        updated["FileSystem"]["WindowsConfiguration"]
    );

    // An update that returns a provisioned configuration to `AUTOMATIC` puts it back on
    // the rate: 3 * 2000.
    tokio::time::sleep(PAST_TRANSITION).await;
    let updated = ok(
        &router,
        "UpdateFileSystem",
        &json!({
            "FileSystemId": provisioned_id,
            "WindowsConfiguration": {
                "DiskIopsConfiguration": { "Mode": MODE_AUTOMATIC },
            },
        }),
    )
    .await;
    assert_eq!(
        disk_iops(&updated["FileSystem"], "WindowsConfiguration"),
        Some((MODE_AUTOMATIC.to_string(), Some(6000))),
        "case \"WINDOWS returned to AUTOMATIC at 2000 GiB\": expected Iops 6000, got {:?}",
        updated["FileSystem"]["WindowsConfiguration"]
    );

    // ONTAP is the one configuration whose HA pairs multiply the rate, so an update that
    // changes them moves the automatic count: 3 * 1024 * 2.
    let created = ok(
        &router,
        "CreateFileSystem",
        &create_request(
            "ONTAP",
            "OntapConfiguration",
            &json!({ "DeploymentType": "SINGLE_AZ_2", "ThroughputCapacityPerHAPair": 1536 }),
            1024,
            &subnets,
        ),
    )
    .await;
    let ontap_id = created["FileSystem"]["FileSystemId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        disk_iops(&created["FileSystem"], "OntapConfiguration"),
        Some((MODE_AUTOMATIC.to_string(), Some(3072))),
        "case \"ONTAP 1024 GiB, default HA pairs\": expected Iops 3072, got {:?}",
        created["FileSystem"]["OntapConfiguration"]
    );
    tokio::time::sleep(PAST_TRANSITION).await;
    let updated = ok(
        &router,
        "UpdateFileSystem",
        &json!({
            "FileSystemId": ontap_id,
            "OntapConfiguration": { "HAPairs": 2 },
        }),
    )
    .await;
    assert_eq!(
        disk_iops(&updated["FileSystem"], "OntapConfiguration"),
        Some((MODE_AUTOMATIC.to_string(), Some(6144))),
        "case \"ONTAP 1024 GiB on 2 HA pairs\": expected Iops 6144, got {:?}",
        updated["FileSystem"]["OntapConfiguration"]
    );
}

/// RULE: the default belongs to the configurations that MODEL the member. The modelled
/// `LustreFileSystemConfiguration` has no `DiskIopsConfiguration` at all — its SSD IOPS
/// live on `FileSystemLustreMetadataConfiguration`, a metadata-only value with its own
/// modes — so a Lustre file system reports none.
#[tokio::test]
async fn a_lustre_file_system_reports_no_disk_iops_configuration() {
    let router = emulator().await;
    for deployment_type in ["SCRATCH_2", "PERSISTENT_1", "PERSISTENT_2"] {
        let subnets = [router.subnet_a()];
        let created = ok(
            &router,
            "CreateFileSystem",
            &create_request(
                "LUSTRE",
                "LustreConfiguration",
                &json!({
                    "DeploymentType": deployment_type,
                    "PerUnitStorageThroughput": 50,
                }),
                1200,
                &subnets,
            ),
        )
        .await;
        let file_system = created["FileSystem"].clone();
        assert_eq!(
            disk_iops(&file_system, "LustreConfiguration"),
            None,
            "case {deployment_type:?}: expected no DiskIopsConfiguration on a Lustre \
             configuration, got {:?}",
            file_system["LustreConfiguration"]
        );
    }
}

/// How long a wait below gives a clock-owed change to be materialised: far longer than the
/// service's own transition span, so what each wait is bounded by is the CONDITION and not
/// how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
/// How long a wait below pauses between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Read the file system `id` back until `settled` holds of what `DescribeFileSystems`
/// reports, failing the case with `expectation` once [`POLL_DEADLINE`] has passed without
/// it.
async fn file_system_until(
    router: &MultiServiceRouter,
    id: &str,
    case: &str,
    expectation: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let started = Instant::now();
    loop {
        let described = describe_one(router, id).await;
        if settled(&described) {
            return described;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected the file system {id} to {expectation}, last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the backup `id` back until `settled` holds of what `DescribeBackups` reports,
/// failing the case with `expectation` once [`POLL_DEADLINE`] has passed without it.
async fn backup_until(
    router: &MultiServiceRouter,
    id: &str,
    case: &str,
    expectation: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let started = Instant::now();
    loop {
        let described = describe_backup(router, id).await;
        if settled(&described) {
            return described;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected the backup {id} to {expectation}, last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the backup table: a file system, the SSD IOPS it has when the backup is
/// taken, the capacity it is grown to afterwards and the IOPS it has once that growth has
/// been carried out.
struct BackupRow {
    /// The modelled `FileSystemType` value.
    file_system_type: &'static str,
    /// The `FileSystem` member the per-type configuration is reported in.
    member: &'static str,
    /// The create request's per-type configuration.
    configuration: Value,
    /// `StorageCapacity` at create time, in GiB.
    storage_capacity: i64,
    /// The `(Mode, Iops)` the file system — and therefore the backup taken of it — has at
    /// that capacity.
    at_backup: (&'static str, i64),
    /// The capacity the source is grown to afterwards, clearing the documented
    /// "at least 10 percent greater than the current storage capacity value" floor.
    grown_capacity: i64,
    /// The `(Mode, Iops)` the SOURCE has once that growth has been carried out: the
    /// automatic rate follows the new capacity, a provisioned value does not move.
    after_growth: (&'static str, i64),
}

/// The table the backup case runs: the three configurations that model
/// `DiskIopsConfiguration`, in both modes.
fn backup_rows() -> Vec<BackupRow> {
    vec![
        BackupRow {
            file_system_type: "OPENZFS",
            member: "OpenZFSConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 64 }),
            storage_capacity: 64,
            // 3 * 64
            at_backup: (MODE_AUTOMATIC, 192),
            grown_capacity: 256,
            // 3 * 256
            after_growth: (MODE_AUTOMATIC, 768),
        },
        BackupRow {
            file_system_type: "WINDOWS",
            member: "WindowsConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 32 }),
            storage_capacity: 32,
            // 3 * 32
            at_backup: (MODE_AUTOMATIC, 96),
            grown_capacity: 2000,
            // 3 * 2000
            after_growth: (MODE_AUTOMATIC, 6000),
        },
        BackupRow {
            file_system_type: "WINDOWS",
            member: "WindowsConfiguration",
            configuration: json!({
                "DeploymentType": "SINGLE_AZ_1",
                "ThroughputCapacity": 32,
                "DiskIopsConfiguration": { "Mode": MODE_USER_PROVISIONED, "Iops": 1200 },
            }),
            storage_capacity: 32,
            at_backup: (MODE_USER_PROVISIONED, 1200),
            grown_capacity: 2000,
            // A provisioned count is the caller's: growing the capacity leaves it alone.
            after_growth: (MODE_USER_PROVISIONED, 1200),
        },
        BackupRow {
            file_system_type: "ONTAP",
            member: "OntapConfiguration",
            configuration: json!({ "DeploymentType": "SINGLE_AZ_1", "ThroughputCapacity": 128 }),
            storage_capacity: 1024,
            // 3 * 1024 * the default 1 HA pair
            at_backup: (MODE_AUTOMATIC, 3072),
            grown_capacity: 2048,
            // 3 * 2048 * 1
            after_growth: (MODE_AUTOMATIC, 6144),
        },
    ]
}

/// RULE: a backup records the file system it was taken OF, so the `DiskIopsConfiguration`
/// it embeds is the one that file system HAD at that moment — reported by `CreateBackup`
/// itself as well as by every later `DescribeBackups`, and left exactly as recorded when
/// the source file system's own capacity, and with it its automatic IOPS, moves afterwards.
#[tokio::test]
async fn a_backup_embeds_the_disk_iops_the_file_system_had_when_it_was_taken() {
    let router = emulator().await;
    for row in backup_rows() {
        let case = format!(
            "{} {} GiB {}",
            row.file_system_type, row.storage_capacity, row.configuration
        );
        let subnets = [router.subnet_a(), router.subnet_b()];
        let created = ok(
            &router,
            "CreateFileSystem",
            &create_request(
                row.file_system_type,
                row.member,
                &row.configuration,
                row.storage_capacity,
                &subnets,
            ),
        )
        .await;
        let file_system_id = created["FileSystem"]["FileSystemId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let recorded = Some((row.at_backup.0.to_string(), Some(row.at_backup.1)));

        // A backup is taken of a file system that is in service, so the case waits for the
        // lifecycle the create reported as still owed.
        file_system_until(
            &router,
            &file_system_id,
            &case,
            "reach AVAILABLE",
            |described| described["Lifecycle"] == "AVAILABLE",
        )
        .await;

        // `CreateBackup`'s own answer embeds the file system it is of.
        let backup = ok(
            &router,
            "CreateBackup",
            &with_client_request_token(&json!({ "FileSystemId": file_system_id })),
        )
        .await["Backup"]
            .clone();
        let backup_id = backup["BackupId"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            disk_iops(&backup["FileSystem"], row.member),
            recorded,
            "case {case:?}: expected CreateBackup to embed the file system's {recorded:?}, got \
             {:?}",
            backup["FileSystem"][row.member]
        );

        // The stored backup, read back once the clock has taken it into service.
        let described = backup_until(&router, &backup_id, &case, "reach AVAILABLE", |backup| {
            backup["Lifecycle"] == "AVAILABLE"
        })
        .await;
        assert_eq!(
            disk_iops(&described["FileSystem"], row.member),
            recorded,
            "case {case:?}: expected DescribeBackups to report the recorded {recorded:?}, got \
             {:?}",
            described["FileSystem"][row.member]
        );
        assert_eq!(
            described["FileSystem"]["StorageCapacity"],
            json!(row.storage_capacity),
            "case {case:?}: expected the backup to record the capacity the file system had, \
             got {:?}",
            described["FileSystem"]["StorageCapacity"]
        );

        // The source grows. Its automatic IOPS follow its new capacity, which is carried
        // out by the administrative action the update records rather than in line, so the
        // case waits for the capacity itself to move.
        ok(
            &router,
            "UpdateFileSystem",
            &json!({
                "FileSystemId": file_system_id,
                "StorageCapacity": row.grown_capacity,
            }),
        )
        .await;
        let grown = file_system_until(
            &router,
            &file_system_id,
            &case,
            "report the capacity it was grown to",
            |described| described["StorageCapacity"] == json!(row.grown_capacity),
        )
        .await;
        assert_eq!(
            disk_iops(&grown, row.member),
            Some((row.after_growth.0.to_string(), Some(row.after_growth.1))),
            "case {case:?}: expected the grown file system to report {:?}, got {:?}",
            row.after_growth,
            grown[row.member]
        );

        // The backup is of what the file system WAS: the source's growth does not reach it.
        // It is already in service by now, so this is a plain read and not a wait.
        let described = describe_backup(&router, &backup_id).await;
        assert_eq!(
            disk_iops(&described["FileSystem"], row.member),
            recorded,
            "case {case:?}: expected the backup to still report the recorded {recorded:?} after \
             the source grew, got {:?}",
            described["FileSystem"][row.member]
        );
        assert_eq!(
            described["FileSystem"]["StorageCapacity"],
            json!(row.storage_capacity),
            "case {case:?}: expected the backup to still record the capacity it was taken at, \
             got {:?}",
            described["FileSystem"]["StorageCapacity"]
        );
    }
}
