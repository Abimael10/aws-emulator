// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `AllocatedStorage` a DB instance may be CREATED with, which
//! Amazon RDS bounds per (ENGINE, STORAGE TYPE) pair.
//!
//! The rule under test: a request that NAMES a `StorageType` is held to the size range that
//! type publishes for the request's engine — `AllocatedStorage` carries one constraint list
//! per engine and one entry per storage type in it ("General Purpose (SSD) storage (gp2,
//! gp3): Must be an integer from 20 to 65536", "Provisioned IOPS storage (io1, io2): Must be
//! an integer from 100 to 65536", "Magnetic storage (standard): Must be an integer from 5 to
//! 3072", from 10 for the Oracle engines and from 20 to 1024 for the SQL Server ones, whose
//! own cap is 16384 GiB) — and a size outside it is refused with HTTP 400 and
//! `InvalidParameterValue`, naming the engine, the storage type and the size, with nothing
//! created. A request that names NO type leaves the type to the service, which places the
//! volume on a type that can carry the size, so it is refused only for a size no type that
//! engine publishes could carry — which is why a size under the default type's own floor is
//! accepted.
//!
//! The table spans what the rule generalises over: the storage types whose floors differ
//! (general purpose, provisioned IOPS), the ceiling the ENGINE caps a type at, requests that
//! name no type at all on three engines whose lowest published floor differs, the same with
//! `Iops` supplied (which changes the type such a request is placed on), and for each bound
//! both the published value and one step past it.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST, and every
//! assertion reads the XML the emulator answered with.

use service_com_amazonaws_rds_amazonrdsv19::ops::describe_orderable_db_instance_options::{
    allocated_storage_range, widest_allocated_storage_range,
};

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};

/// The error code Amazon RDS answers a parameter value its member does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The fault a report answers for a DB instance that does not exist.
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// A master user every row carries, inside every engine's published bound, so the only
/// refusal in play is the one under test.
const MASTER_USER: &str = "emuadmin";

/// A master user password inside every engine's published policy, for the same reason.
const PASSWORD: &str = "Emulator-Probe-1";

/// What a row expects of the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The size is inside the range that applies: created, and reported back.
    Accepted,
    /// The size is outside it: refused, and nothing created.
    Refused,
}

/// One (engine, storage type, size) a create is made with, and what the published ranges say
/// about it.
struct Row {
    case: &'static str,
    engine: &'static str,
    class: &'static str,
    /// `StorageType`, or `None` for a request that names none.
    storage_type: Option<&'static str>,
    /// `Iops`, which is what the documented default storage type turns on.
    iops: Option<i32>,
    allocated_storage: i32,
    outcome: Outcome,
    /// The storage type the volume ends up on, which the refusal names and the created
    /// instance reports.
    effective_storage_type: &'static str,
}

/// Each class of bound the rule covers, on both sides of the published value.
///
/// Source: the per-engine constraint lists of `CreateDBInstanceMessage$AllocatedStorage`
/// (`smithy_query(query="doc", shape="CreateDBInstanceMessage", member="AllocatedStorage")`),
/// read back from the service's own published table through [`allocated_storage_range`] and
/// [`widest_allocated_storage_range`], which every row cross-checks.
const ROWS: &[Row] = &[
    Row {
        case: "general purpose SSD below its floor",
        engine: "postgres",
        class: "db.t3.micro",
        storage_type: Some("gp3"),
        iops: None,
        allocated_storage: 5,
        outcome: Outcome::Refused,
        effective_storage_type: "gp3",
    },
    Row {
        case: "general purpose SSD at its floor",
        engine: "postgres",
        class: "db.t3.micro",
        storage_type: Some("gp3"),
        iops: None,
        allocated_storage: 20,
        outcome: Outcome::Accepted,
        effective_storage_type: "gp3",
    },
    Row {
        case: "general purpose SSD one below its floor",
        engine: "mysql",
        class: "db.t3.micro",
        storage_type: Some("gp2"),
        iops: None,
        allocated_storage: 19,
        outcome: Outcome::Refused,
        effective_storage_type: "gp2",
    },
    Row {
        case: "provisioned IOPS SSD below its floor",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: Some("io1"),
        iops: Some(1_000),
        allocated_storage: 50,
        outcome: Outcome::Refused,
        effective_storage_type: "io1",
    },
    Row {
        case: "provisioned IOPS SSD at its floor",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: Some("io2"),
        iops: Some(1_000),
        allocated_storage: 100,
        outcome: Outcome::Accepted,
        effective_storage_type: "io2",
    },
    Row {
        case: "general purpose SSD one past the type's own ceiling",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: Some("gp2"),
        iops: None,
        allocated_storage: 65_537,
        outcome: Outcome::Refused,
        effective_storage_type: "gp2",
    },
    Row {
        case: "general purpose SSD at the ceiling its ENGINE caps the type at",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage_type: Some("gp3"),
        iops: None,
        allocated_storage: 16_384,
        outcome: Outcome::Accepted,
        effective_storage_type: "gp3",
    },
    Row {
        case: "general purpose SSD one past the ceiling its ENGINE caps the type at",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage_type: Some("gp3"),
        iops: None,
        allocated_storage: 16_385,
        outcome: Outcome::Refused,
        effective_storage_type: "gp3",
    },
    Row {
        case: "no storage type named, under the default type's floor but inside a published \
               range",
        engine: "mariadb",
        class: "db.t3.medium",
        storage_type: None,
        iops: None,
        allocated_storage: 10,
        outcome: Outcome::Accepted,
        effective_storage_type: "gp2",
    },
    Row {
        case: "no storage type named, below every range the engine publishes",
        engine: "mariadb",
        class: "db.t3.medium",
        storage_type: None,
        iops: None,
        allocated_storage: 4,
        outcome: Outcome::Refused,
        effective_storage_type: "gp2",
    },
    Row {
        case: "no storage type named, below every range the ORACLE engines publish",
        engine: "oracle-se2",
        class: "db.m5.xlarge",
        storage_type: None,
        iops: None,
        allocated_storage: 5,
        outcome: Outcome::Refused,
        effective_storage_type: "gp2",
    },
    Row {
        case: "no storage type named, at the lowest range the ORACLE engines publish",
        engine: "oracle-se2",
        class: "db.m5.xlarge",
        storage_type: None,
        iops: None,
        allocated_storage: 10,
        outcome: Outcome::Accepted,
        effective_storage_type: "gp2",
    },
    Row {
        case: "no storage type named, one below every range the SQL SERVER engines publish",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage_type: None,
        iops: None,
        allocated_storage: 19,
        outcome: Outcome::Refused,
        effective_storage_type: "gp2",
    },
    Row {
        case: "Iops supplied with no storage type, under the provisioned type's floor",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: None,
        iops: Some(1_000),
        allocated_storage: 50,
        outcome: Outcome::Accepted,
        effective_storage_type: "io1",
    },
    Row {
        case: "Iops supplied with no storage type, past every range the engine publishes",
        engine: "postgres",
        class: "db.m5.large",
        storage_type: None,
        iops: Some(1_000),
        allocated_storage: 65_537,
        outcome: Outcome::Refused,
        effective_storage_type: "io1",
    },
];

/// The create one row makes.
fn create_params(row: &Row, identifier: &str) -> Vec<(&'static str, String)> {
    let mut params: Vec<(&'static str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", row.class.to_string()),
        ("AllocatedStorage", row.allocated_storage.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", PASSWORD.to_string()),
    ];
    if let Some(storage_type) = row.storage_type {
        params.push(("StorageType", storage_type.to_string()));
    }
    if let Some(iops) = row.iops {
        params.push(("Iops", iops.to_string()));
    }
    params
}

/// The range a row's request is held to: the named type's, or the widest the engine's types
/// reach between them when the request names none.
fn applicable_range(row: &Row) -> (i32, i32) {
    match row.storage_type {
        Some(named) => allocated_storage_range(row.engine, named).unwrap_or_else(|| {
            panic!(
                "case {:?}: the engine and storage type must publish a size range",
                row.case
            )
        }),
        None => widest_allocated_storage_range(row.engine).unwrap_or_else(|| {
            panic!(
                "case {:?}: the engine must publish a size range for some storage type",
                row.case
            )
        }),
    }
}

/// RULE: an `AllocatedStorage` is held to the range its storage type publishes for the
/// engine, or — where the request names no type — to the widest range that engine's types
/// reach: accepted and stored inside it, refused with `InvalidParameterValue` naming the
/// engine, the type and the size outside it, with nothing created.
#[tokio::test]
async fn allocated_storage_is_held_to_the_range_its_engine_and_storage_type_publish() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        // The range the service reads and the outcome this suite states agree: a drift in
        // the published table is a failure here rather than a silently weaker assertion.
        let (min, max) = applicable_range(row);
        let inside = (min..=max).contains(&row.allocated_storage);
        let expected_inside = row.outcome == Outcome::Accepted;
        assert_eq!(
            inside, expected_inside,
            "case {:?}: expected {} to sit {} the published range {min}..={max}",
            row.case,
            row.allocated_storage,
            if expected_inside { "inside" } else { "outside" }
        );

        let identifier = format!("emu-storage-floor-{index}");
        let params = create_params(row, &identifier);
        let response = call(&router, "CreateDBInstance", &params).await;

        match row.outcome {
            Outcome::Accepted => {
                let created = expect_ok(row.case, response);
                let reported = text(&created, "AllocatedStorage");
                assert_eq!(
                    reported,
                    row.allocated_storage.to_string(),
                    "case {:?}: expected the created instance to report the size asked for \
                     ({}), got {reported:?}",
                    row.case,
                    row.allocated_storage
                );

                // ...and the STORE agrees: the size is read back through the report.
                let described = expect_ok(
                    row.case,
                    call(
                        &router,
                        "DescribeDBInstances",
                        &[("DBInstanceIdentifier", identifier.clone())],
                    )
                    .await,
                );
                let stored = text(&described, "AllocatedStorage");
                assert_eq!(
                    stored,
                    row.allocated_storage.to_string(),
                    "case {:?}: expected the stored size to be the one asked for ({}), got \
                     {stored:?}",
                    row.case,
                    row.allocated_storage
                );
                let stored_type = text(&described, "StorageType");
                assert_eq!(
                    stored_type, row.effective_storage_type,
                    "case {:?}: expected the stored storage type {:?}, got {stored_type:?}",
                    row.case, row.effective_storage_type
                );
            }
            Outcome::Refused => {
                let refused = expect_error(row.case, response, 400, INVALID_PARAMETER_VALUE);
                let message = text(&refused, "Message");
                let expected = format!(
                    "Invalid storage size for engine name {} and storage type {}: {}",
                    row.engine, row.effective_storage_type, row.allocated_storage
                );
                assert_eq!(
                    message, expected,
                    "case {:?}: expected the refusal {expected:?}, got {message:?}",
                    row.case
                );

                // Nothing was created: the report answers the identifier's not-found fault.
                let not_found = expect_error(
                    row.case,
                    call(
                        &router,
                        "DescribeDBInstances",
                        &[("DBInstanceIdentifier", identifier.clone())],
                    )
                    .await,
                    404,
                    DB_INSTANCE_NOT_FOUND,
                );
                let message = text(&not_found, "Message");
                let expected = format!("DBInstance {identifier} not found.");
                assert_eq!(
                    message, expected,
                    "case {:?}: expected the report to answer {expected:?}, got {message:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: the range is the named storage type's, so one size is accepted on a type whose range
/// admits it and refused on one whose range does not — the two answers differ in the storage
/// type alone.
#[tokio::test]
async fn one_size_is_accepted_or_refused_by_the_storage_type_alone() {
    let router = emulator();

    // Fifty gibibytes: inside the general purpose SSD range (from 20), below the provisioned
    // IOPS SSD floor (100).
    const SIZE: i32 = 50;
    let base: [(&str, String); 5] = [
        ("Engine", "postgres".to_string()),
        ("DBInstanceClass", "db.m5.large".to_string()),
        ("AllocatedStorage", SIZE.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", PASSWORD.to_string()),
    ];

    let mut admitted = base.to_vec();
    admitted.push(("DBInstanceIdentifier", "emu-storage-pair-gp3".to_string()));
    admitted.push(("StorageType", "gp3".to_string()));
    let created = expect_ok(
        "general purpose SSD admits the size",
        call(&router, "CreateDBInstance", &admitted).await,
    );
    let reported = text(&created, "AllocatedStorage");
    assert_eq!(
        reported,
        SIZE.to_string(),
        "case {:?}: expected the size to be accepted and reported, got {reported:?}",
        "general purpose SSD admits the size"
    );

    let mut refused = base.to_vec();
    refused.push(("DBInstanceIdentifier", "emu-storage-pair-io1".to_string()));
    refused.push(("StorageType", "io1".to_string()));
    let body = expect_error(
        "provisioned IOPS SSD refuses the same size",
        call(&router, "CreateDBInstance", &refused).await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    let expected =
        format!("Invalid storage size for engine name postgres and storage type io1: {SIZE}");
    assert_eq!(
        message, expected,
        "case {:?}: expected the refusal {expected:?}, got {message:?}",
        "provisioned IOPS SSD refuses the same size"
    );
}
