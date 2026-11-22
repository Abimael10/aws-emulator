// SPDX-License-Identifier: CC0-1.0

//! Whether a DB instance is reported to support a DEDICATED LOG VOLUME, over a table of
//! instances whose STORAGE differs.
//!
//! RULE: a dedicated log volume carries a DB instance's own transaction logs, and it is
//! available to an instance that uses Provisioned IOPS SSD storage (the "Dedicated log
//! volume (DLV)" section of the Amazon RDS User Guide), so
//! `ValidDBInstanceModificationsMessage$SupportsDedicatedLogVolume` is the answer the
//! storage type the instance RUNS publishes — not its engine's and not its class's. General
//! Purpose SSD and legacy magnetic storage take none, and storage a DB CLUSTER manages takes
//! none either, since no volume of it is ordered with the instance at all.
//!
//! The rows differ along everything the rule generalises over: each storage type
//! `CreateDBInstanceMessage$StorageType` publishes for an instance that owns its storage
//! ("Valid Values: `gp2 | gp3 | io1 | io2 | standard`"), the type a create defaults to when
//! the request names none, the cluster-managed type of an Aurora instance, and several
//! engines carrying the same type — so no row's answer can come from its engine.
//!
//! Source: `smithy_query(query="doc", shape="ValidDBInstanceModificationsMessage",
//! member="SupportsDedicatedLogVolume")` for the member, and
//! `smithy_query(query="doc", shape="CreateDBInstanceMessage", member="StorageType")` for the
//! types an instance can carry.

use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};

/// One instance to create, and what the report must say about its storage.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this case is about, as the assertion names it.
    case: &'static str,
    /// The engine the instance runs.
    engine: &'static str,
    /// The DB instance class it runs on.
    class: &'static str,
    /// `StorageType`, or `None` for a create that names none — which is the type its
    /// engine defaults to.
    storage_type: Option<&'static str>,
    /// `AllocatedStorage`, in GiB: inside the published range of the type the row names.
    allocated_storage: Option<i64>,
    /// `Iops`, which Provisioned IOPS SSD storage is ordered with.
    iops: Option<i64>,
    /// The `StorageType` the instance must REPORT once created: the type the request named,
    /// or the one its create defaults to.
    reported_storage_type: &'static str,
    /// Whether that type takes a dedicated log volume.
    dedicated_log_volume: bool,
}

/// The instances this suite reports on.
const ROWS: &[Row] = &[
    Row {
        case: "the storage type a create defaults to",
        engine: "mysql",
        class: "db.m5.large",
        storage_type: None,
        allocated_storage: Some(100),
        iops: None,
        // `CreateDBInstanceMessage$StorageType`'s own default for a request naming neither
        // a type nor `Iops`, which this service provisions as General Purpose SSD.
        reported_storage_type: "gp2",
        dedicated_log_volume: false,
    },
    Row {
        case: "General Purpose SSD gp2",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: Some("gp2"),
        allocated_storage: Some(100),
        iops: None,
        reported_storage_type: "gp2",
        dedicated_log_volume: false,
    },
    Row {
        case: "General Purpose SSD gp3",
        engine: "mariadb",
        class: "db.t3.micro",
        storage_type: Some("gp3"),
        allocated_storage: Some(100),
        iops: None,
        reported_storage_type: "gp3",
        dedicated_log_volume: false,
    },
    Row {
        case: "Provisioned IOPS SSD io1",
        engine: "mysql",
        class: "db.m5.large",
        storage_type: Some("io1"),
        allocated_storage: Some(100),
        iops: Some(1_000),
        reported_storage_type: "io1",
        dedicated_log_volume: true,
    },
    Row {
        // The same type on another engine and a burstable class: the answer is the
        // storage's, so neither moves it.
        case: "Provisioned IOPS SSD io1 on another engine",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage_type: Some("io1"),
        allocated_storage: Some(100),
        iops: Some(1_000),
        reported_storage_type: "io1",
        dedicated_log_volume: true,
    },
    Row {
        case: "Provisioned IOPS SSD io2",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: Some("io2"),
        allocated_storage: Some(200),
        iops: Some(2_000),
        reported_storage_type: "io2",
        dedicated_log_volume: true,
    },
    Row {
        case: "legacy magnetic storage",
        engine: "mysql",
        class: "db.m5.large",
        storage_type: Some("standard"),
        allocated_storage: Some(100),
        iops: None,
        reported_storage_type: "standard",
        dedicated_log_volume: false,
    },
    Row {
        // An Aurora instance's storage is its CLUSTER's: "This setting doesn't apply to
        // Amazon Aurora DB instances. Storage is managed by the DB cluster", and the type it
        // carries is one of the cluster's own ("Aurora DB clusters - `aurora` |
        // `aurora-iopt1`"), of which no volume is ordered with the instance.
        case: "storage a DB cluster manages",
        engine: "aurora-mysql",
        class: "db.r5.large",
        storage_type: Some("aurora"),
        allocated_storage: None,
        iops: None,
        reported_storage_type: "aurora",
        dedicated_log_volume: false,
    },
];

/// The identifier the instance of row `index` is created under.
fn identifier(index: usize) -> String {
    format!("emu-dlv-{index}")
}

/// `SupportsDedicatedLogVolume` as `DescribeValidDBInstanceModifications` reports it for one
/// instance.
async fn reported_support(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    named: &str,
) -> Option<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeValidDBInstanceModifications",
            &[("DBInstanceIdentifier", named.to_string())],
        )
        .await,
    );
    let message = subtrees(&body, "ValidDBInstanceModificationsMessage")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the response carries no report at all: {body}")
        });
    optional_text(&message, "SupportsDedicatedLogVolume")
}

/// The `StorageType` the stored instance reports, read back through `DescribeDBInstances`.
async fn stored_storage_type(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    named: &str,
) -> String {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", named.to_string())],
        )
        .await,
    );
    text(&body, "StorageType")
}

#[tokio::test]
async fn the_dedicated_log_volume_answer_is_the_instances_own_storage_types() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let named = identifier(index);
        let mut params: Vec<(&str, String)> = vec![
            ("DBInstanceIdentifier", named.clone()),
            ("Engine", row.engine.to_string()),
            ("DBInstanceClass", row.class.to_string()),
        ];
        if let Some(storage_type) = row.storage_type {
            params.push(("StorageType", storage_type.to_string()));
        }
        if let Some(allocated_storage) = row.allocated_storage {
            params.push(("AllocatedStorage", allocated_storage.to_string()));
        }
        if let Some(iops) = row.iops {
            params.push(("Iops", iops.to_string()));
        }
        expect_ok(case, call(&router, "CreateDBInstance", &params).await);

        // The storage the instance really carries, read back through the describe: the
        // answer below is a fact about THIS storage, so the row's expectation is anchored to
        // what the created instance reports rather than to what the request asked for.
        let storage_type = stored_storage_type(&router, case, &named).await;
        assert_eq!(
            storage_type, row.reported_storage_type,
            "case {case:?}: expected the instance to run {:?} storage, got {storage_type:?}",
            row.reported_storage_type
        );
        let reported = reported_support(&router, case, &named).await;
        assert_eq!(
            reported.as_deref(),
            Some(if row.dedicated_log_volume {
                "true"
            } else {
                "false"
            }),
            "case {case:?}: expected SupportsDedicatedLogVolume {} for {storage_type:?} \
             storage, got {reported:?}",
            row.dedicated_log_volume
        );
    }
}
