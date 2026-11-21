// SPDX-License-Identifier: CC0-1.0

//! On-demand DB backups, end to end over the wire.
//!
//! The suite drives real `awsJson1_0` requests through the same [`MultiServiceRouter`] the
//! production router dispatches with, reusing the harness in
//! [`crate::db_instance_lifecycle`] (`router_and_engine`, `call`, `call_in`, `ok`) so there
//! is one wire harness for this crate.
//!
//! The assertions are RULES over a table of inputs, not a replay of one recorded call:
//!
//! * `CreateDbBackup` mints a backup resource of the account and Region the call is made
//!   in, and every member it reports "at the time of backup" is a SNAPSHOT of the DB
//!   instance or DB cluster `dbResourceId` names, copied off that resource's stored record
//!   — so the backup keeps reporting what it captured however the source changes later,
//!   and `GetDbBackup` reads the same record back member for member.
//! * `expiresAfter` is the calendar date `retentionDays` days after the backup was taken,
//!   and is reported only when the create asked for a retention at all.
//! * A backup starts `IN_PROGRESS` and the CLOCK settles it to `COMPLETED`.
//! * A backup's name is unique within the account and Region: a second create under a
//!   stored name is refused `ConflictException`, the same name in another Region is a new
//!   backup, and a create naming a DB resource this account holds none of is refused
//!   `ResourceNotFoundException` and stores nothing.
//! * `ListDbBackups` REPORTS the backups the account holds in the Region the call is made
//!   in, each as the summary projection of its own stored record, narrowed to the DB
//!   resource `dbResourceId` names when the request names one, yielding every such record
//!   exactly once across pages of at most `maxResults` items.
//!
//! The rows span what the rules generalise over: which family the source belongs to (DB
//! instance or DB cluster), the engine that resource runs and whether its parameter group
//! is one the account created or one of the service-managed catalog's, how many optional
//! members the source carries, the Region, the name's length inside the modelled
//! `DbBackupName` bounds, the retention at both published bounds and absent, and whether
//! the create attached tags.
//!
//! Every status and type named here is a member of the modelled enum that declares it
//! (`smithy_query(query="shape", shape="DbBackupStatus,DbBackupType,EngineType")`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_timestreaminfluxdb_amazontimestreaminfluxdb as influxdb;

use crate::db_instance_lifecycle::{call_in, ok, router_and_engine, ACCOUNT, REGION};

/// A second Region, so the rules that are per-Region (the ARN's Region segment, the name
/// uniqueness index) are asserted across two of them.
const OTHER_REGION: &str = "eu-central-1";

/// How long this suite waits for the clock-owed settle to `COMPLETED` before giving up:
/// far above the span the service settles a transient status after, so a loaded host
/// cannot fail a test that is about the settle happening at all.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a polling client waits before looking again.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// Seconds in a day, for the date `retentionDays` days of retention put a backup beyond.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// The published bounds of the modelled `RetentionDays` range (1..3650),
/// `smithy_query(query="shape", shape="RetentionDays")`.
const RETENTION_DAYS_RANGE: (i64, i64) = (1, 3650);

/// Members `CreateDbBackupOutput` and the read of the source resource BOTH model, from
/// `smithy_query(query="operation", operation="CreateDbBackup,GetDbInstance,GetDbCluster")`:
/// `(the member the backup reports, the member the source reports it as)`. A backup's value
/// for one of these must be the source's own.
const SNAPSHOT_MEMBERS: &[(&str, &str)] = &[
    ("allocatedStorage", "allocatedStorage"),
    ("dbInstanceType", "dbInstanceType"),
    ("dbStorageType", "dbStorageType"),
    ("deploymentType", "deploymentType"),
    (
        "influxAuthParametersSecretArn",
        "influxAuthParametersSecretArn",
    ),
    ("kmsKeyId", "kmsKeyId"),
    ("logDeliveryConfiguration", "logDeliveryConfiguration"),
    ("maintenanceSchedule", "maintenanceSchedule"),
    ("networkType", "networkType"),
    ("port", "port"),
    ("publiclyAccessible", "publiclyAccessible"),
    ("vpcSecurityGroupIds", "vpcSecurityGroupIds"),
    ("vpcSubnetIds", "vpcSubnetIds"),
];

/// The snapshot members only a DB cluster has: a DB instance has no node set and no
/// failover mode, so `ClusterConfiguration` and `FailoverMode` describe nothing about one
/// (`smithy_query(query="shape", shape="ClusterConfiguration,FailoverMode")`).
const CLUSTER_ONLY_SNAPSHOT_MEMBERS: &[&str] = &["clusterConfiguration", "failoverMode"];

/// Which family the resource a row backs up belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceFamily {
    DbInstance,
    DbCluster,
}

impl SourceFamily {
    /// The operation that creates a resource of this family.
    fn create_operation(self) -> &'static str {
        match self {
            Self::DbInstance => "CreateDbInstance",
            Self::DbCluster => "CreateDbCluster",
        }
    }

    /// The operation that reads one back, and the input member it takes the id in.
    fn read(self) -> (&'static str, &'static str) {
        match self {
            Self::DbInstance => ("GetDbInstance", "identifier"),
            Self::DbCluster => ("GetDbCluster", "dbClusterId"),
        }
    }

    /// The member the source reports the DB parameter group in force on it as: a cluster
    /// reports both the group asked for and the one actually APPLIED, and it is the
    /// applied one a backup captures.
    fn parameter_group_member(self) -> &'static str {
        match self {
            Self::DbInstance => "dbParameterGroupIdentifier",
            Self::DbCluster => "effectiveDbParameterGroupIdentifier",
        }
    }
}

/// One row: a source resource to create, and the backup to take of it.
struct BackupCase {
    label: &'static str,
    region: &'static str,
    family: SourceFamily,
    /// The create request for the source resource.
    source: Value,
    /// The name the backup is created under.
    backup_name: &'static str,
    /// The retention the create asks for, or `None` for a create that asks for none.
    retention_days: Option<i64>,
    tags: Vec<(&'static str, &'static str)>,
    /// The engine the source resource runs, a member of the modelled `EngineType` enum —
    /// the one configured by the DB parameter group applied to it.
    expected_engine: &'static str,
}

/// A router whose account already holds the AWS Marketplace subscription a DB cluster is
/// gated behind, so a row can back one up. A buyer subscribes in AWS Marketplace, out of
/// band of every operation here.
fn backup_router() -> MultiServiceRouter {
    let (router, engine) = router_and_engine();
    influxdb::state::subscribe_account_to_marketplace_offering(
        &*engine,
        influxdb::state::READ_REPLICAS_OFFERING,
    )
    .expect("the shared engine holds this service's store");
    router
}

/// The minimum a `CreateDbInstance` request must carry, per the model's `@required`
/// members (`name`, `password`, `dbInstanceType`, `allocatedStorage`, `vpcSubnetIds`,
/// `vpcSecurityGroupIds`).
fn minimal_instance(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "allocatedStorage": 20,
        "vpcSubnetIds": ["subnet-aaa"],
        "vpcSecurityGroupIds": ["sg-aaa"],
    })
}

/// The minimum a `CreateDbCluster` request must carry, per the model's `@required`
/// members (`name`, `dbInstanceType`, `vpcSubnetIds`, `vpcSecurityGroupIds`).
fn minimal_cluster(name: &str) -> Value {
    json!({
        "name": name,
        "dbInstanceType": "db.influx.large",
        "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
        "vpcSecurityGroupIds": ["sg-aaa"],
    })
}

/// A string member of a body, panicking with the case and the body when it is not one.
fn string_member(case: &str, body: &Value, member: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string `{member}`, got {body}"))
        .to_string()
}

/// `^[a-zA-Z0-9]+$`, length 3..64 — the modelled `DbBackupId` constraint.
fn is_modelled_db_backup_id(id: &str) -> bool {
    (3..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// `^\d{4}-\d{2}-\d{2}$` — the modelled `Date` pattern `expiresAfter` carries.
fn is_modelled_date(date: &str) -> bool {
    let parts: Vec<&str> = date.split('-').collect();
    parts.len() == 3
        && [4, 2, 2]
            .iter()
            .zip(&parts)
            .all(|(width, part)| part.len() == *width && part.chars().all(|c| c.is_ascii_digit()))
}

/// The `YYYY-MM-DD` calendar date `days` days after the instant `created_at_secs` names.
fn date_after(created_at_secs: i64, days: i64) -> String {
    let instant = ::aws_smithy_types::DateTime::from_secs(created_at_secs + days * SECONDS_PER_DAY);
    let rendered = instant
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant inside the calendar renders");
    rendered
        .get(..10)
        .expect("a rendered instant carries its date")
        .to_string()
}

/// The whole second the `createdAt` of a body falls in.
///
/// `createdAt` targets a `Timestamp`, which this protocol renders as epoch seconds, so the
/// instant arrives as a JSON number.
fn created_at_secs(case: &str, body: &Value) -> i64 {
    let seconds = body["createdAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a numeric `createdAt`, got {body}"));
    seconds.floor() as i64
}

/// A backup record without its `status`: what two reads of the same record must agree on
/// member for member, the status excepted because the CLOCK settles it between them.
fn without_status(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(members) = body.as_object_mut() {
        members.remove("status");
    }
    body
}

/// The statuses a stored backup can be in while this suite watches it: being taken, or
/// taken — both members of the modelled `DbBackupStatus` enum.
const LIVE_BACKUP_STATUSES: &[&str] = &["IN_PROGRESS", "COMPLETED"];

/// Create the source resource of a row and hand back its id.
async fn create_source(router: &MultiServiceRouter, case: &BackupCase) -> String {
    let created = ok(
        case.label,
        call_in(
            router,
            case.region,
            case.family.create_operation(),
            case.source.clone(),
        )
        .await,
    );
    match case.family {
        SourceFamily::DbInstance => string_member(case.label, &created, "id"),
        SourceFamily::DbCluster => string_member(case.label, &created, "dbClusterId"),
    }
}

/// Read the source resource `id` of `family` back.
async fn read_source(
    router: &MultiServiceRouter,
    region: &str,
    family: SourceFamily,
    id: &str,
    case: &str,
) -> Value {
    let (operation, member) = family.read();
    ok(
        case,
        call_in(router, region, operation, json!({ member: id })).await,
    )
}

/// The `CreateDbBackup` request of a row, for the source resource `db_resource_id`.
fn backup_request(case: &BackupCase, db_resource_id: &str) -> Value {
    let mut body = json!({ "name": case.backup_name, "dbResourceId": db_resource_id });
    if let Some(days) = case.retention_days {
        body["retentionDays"] = json!(days);
    }
    if !case.tags.is_empty() {
        body["tags"] = Value::Object(
            case.tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        );
    }
    body
}

/// The rows this suite spans. A row whose source runs on a customer-created DB parameter
/// group creates that group on `router` first, so its minted id can go into the request.
async fn backup_cases(router: &MultiServiceRouter) -> Vec<BackupCase> {
    let setup = "setup: a customer v3 enterprise parameter group";
    let group = ok(
        setup,
        call_in(
            router,
            REGION,
            "CreateDbParameterGroup",
            json!({
                "name": "BackupSource",
                "parameters": {
                    "InfluxDBv3Enterprise": {
                        "ingestQueryInstances": 2,
                        "queryOnlyInstances": 1,
                        "dedicatedCompactor": true
                    }
                },
            }),
        )
        .await,
    );
    let customer_group = string_member(setup, &group, "id");

    vec![
        BackupCase {
            label: "a DB instance with only the required members, no retention, no tags",
            region: REGION,
            family: SourceFamily::DbInstance,
            source: minimal_instance("backup-minimal"),
            // The shortest name the modelled `DbBackupName` length (3..40) allows.
            backup_name: "abc",
            retention_days: None,
            tags: Vec::new(),
            // An instance created without a parameter group runs the default engine.
            expected_engine: "INFLUXDB_V2",
        },
        BackupCase {
            label: "a DB instance with every optional member, the shortest retention",
            region: REGION,
            family: SourceFamily::DbInstance,
            source: json!({
                "name": "backup-full",
                "password": "supersecret1",
                "username": "myuser",
                "organization": "myorg",
                "bucket": "mybucket",
                "dbInstanceType": "db.influx.8xlarge",
                "allocatedStorage": 400,
                "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
                "vpcSecurityGroupIds": ["sg-aaa", "sg-bbb"],
                "publiclyAccessible": true,
                "dbStorageType": "InfluxIOIncludedT3",
                "dbParameterGroupIdentifier": "InfluxDBV3Core",
                "deploymentType": "SINGLE_AZ",
                "networkType": "DUAL",
                "port": 9999,
                "kmsKeyId": "alias/my-key",
                "logDeliveryConfiguration": {
                    "s3Configuration": { "bucketName": "log-bucket", "enabled": true }
                },
                "maintenanceSchedule": {
                    "timezone": "UTC",
                    "preferredMaintenanceWindow": "Sun:23:00-Sun:23:30"
                },
            }),
            // The longest name the modelled `DbBackupName` length (3..40) allows.
            backup_name: "a23456789-123456789-123456789-123456789",
            retention_days: Some(RETENTION_DAYS_RANGE.0),
            tags: vec![("team", "data"), ("env", "test")],
            // A service-managed group of the v3 core family: the engine follows the
            // parameters of the group applied to the instance.
            expected_engine: "INFLUXDB_V3_CORE",
        },
        BackupCase {
            label: "a multi-AZ standby DB instance in another Region, the longest retention",
            region: OTHER_REGION,
            family: SourceFamily::DbInstance,
            source: json!({
                "name": "backup-standby",
                "password": "supersecret1",
                "dbInstanceType": "db.influx.large",
                "allocatedStorage": 100,
                "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
                "vpcSecurityGroupIds": ["sg-aaa"],
                "deploymentType": "WITH_MULTIAZ_STANDBY",
                "dbStorageType": "InfluxIOIncludedT1",
            }),
            backup_name: "standby-backup",
            retention_days: Some(RETENTION_DAYS_RANGE.1),
            tags: vec![("env", "prod")],
            expected_engine: "INFLUXDB_V2",
        },
        BackupCase {
            label: "a DB instance on a customer-created v3 enterprise parameter group",
            region: REGION,
            family: SourceFamily::DbInstance,
            source: {
                let mut body = minimal_instance("backup-customer-group");
                body["dbParameterGroupIdentifier"] = json!(customer_group);
                body
            },
            backup_name: "customer-group-backup",
            retention_days: Some(7),
            tags: Vec::new(),
            expected_engine: "INFLUXDB_V3_ENTERPRISE",
        },
        BackupCase {
            label: "a DB cluster with only the required members",
            region: REGION,
            family: SourceFamily::DbCluster,
            source: minimal_cluster("backup-cluster"),
            backup_name: "cluster-backup",
            retention_days: None,
            tags: vec![("kind", "cluster")],
            expected_engine: "INFLUXDB_V2",
        },
        BackupCase {
            label: "a DB cluster on a service-managed enterprise group, failover off",
            region: REGION,
            family: SourceFamily::DbCluster,
            source: json!({
                "name": "backup-cluster-ha",
                "dbInstanceType": "db.influx.2xlarge",
                "allocatedStorage": 200,
                "vpcSubnetIds": ["subnet-ccc"],
                "vpcSecurityGroupIds": ["sg-ccc", "sg-ddd"],
                "dbParameterGroupIdentifier": "InfluxDBV3Enterprise",
                "failoverMode": "NO_FAILOVER",
                "networkType": "IPV4",
                "dbStorageType": "InfluxIOIncludedT2",
                "publiclyAccessible": false,
                "port": 8181,
                "kmsKeyId": "alias/cluster-key",
            }),
            backup_name: "cluster-ha-backup",
            retention_days: Some(90),
            tags: Vec::new(),
            expected_engine: "INFLUXDB_V3_ENTERPRISE",
        },
    ]
}

#[tokio::test]
async fn a_created_backup_snapshots_the_resource_it_names_and_reads_back() {
    let router = backup_router();
    for case in backup_cases(&router).await {
        let source_id = create_source(&router, &case).await;
        let created = ok(
            case.label,
            call_in(
                &router,
                case.region,
                "CreateDbBackup",
                backup_request(&case, &source_id),
            )
            .await,
        );

        // The backup is a resource of its own: a minted id inside the modelled pattern,
        // an ARN of the backup family scoped to the Region the call was made in, and the
        // name and source id the request named.
        let id = string_member(case.label, &created, "id");
        assert!(
            is_modelled_db_backup_id(&id),
            "case {:?}: expected a minted DbBackupId, got {id:?}",
            case.label
        );
        assert_ne!(
            id, source_id,
            "case {:?}: a backup is its own resource, not the one it was taken from",
            case.label
        );
        assert_eq!(
            created["arn"].as_str(),
            Some(
                format!(
                    "arn:aws:timestream-influxdb:{}:{ACCOUNT}:db-backup/{id}",
                    case.region
                )
                .as_str()
            ),
            "case {:?}: expected the backup's own ARN, got {:?}",
            case.label,
            created["arn"]
        );
        assert_eq!(
            created["name"].as_str(),
            Some(case.backup_name),
            "case {:?}: the record must carry the name asked for, got {created}",
            case.label
        );
        assert_eq!(
            created["dbResourceId"].as_str(),
            Some(source_id.as_str()),
            "case {:?}: the backup must name the resource it was taken from, got {created}",
            case.label
        );
        assert_eq!(
            created["status"].as_str(),
            Some("IN_PROGRESS"),
            "case {:?}: a freshly created backup is being taken, got {created}",
            case.label
        );
        assert_eq!(
            created["type"].as_str(),
            Some("ON_DEMAND"),
            "case {:?}: CreateDbBackup takes an on-demand backup, got {created}",
            case.label
        );
        assert_eq!(
            created["engineType"].as_str(),
            Some(case.expected_engine),
            "case {:?}: expected the engine of the group applied to the source, got {:?}",
            case.label,
            created["engineType"]
        );

        // Every member reported "at the time of backup" is the source's own.
        let source = read_source(&router, case.region, case.family, &source_id, case.label).await;
        for (backup_member, source_member) in SNAPSHOT_MEMBERS {
            assert_eq!(
                created.get(backup_member),
                source.get(source_member),
                "case {:?}: the backup must report the source's {source_member} as \
                 {backup_member}, got {:?} against {:?}",
                case.label,
                created.get(backup_member),
                source.get(source_member)
            );
        }
        assert_eq!(
            created.get("dbParameterGroupId"),
            source.get(case.family.parameter_group_member()),
            "case {:?}: the backup must report the group in force on the source, got {:?}",
            case.label,
            created.get("dbParameterGroupId")
        );
        for member in CLUSTER_ONLY_SNAPSHOT_MEMBERS {
            let expected = match case.family {
                SourceFamily::DbCluster => source.get(member),
                SourceFamily::DbInstance => None,
            };
            assert_eq!(
                created.get(member),
                expected,
                "case {:?}: {member} describes a DB cluster only, got {:?}",
                case.label,
                created.get(member)
            );
        }

        // The reader answers the same record, member for member — the status excepted,
        // which the clock settles on its own schedule.
        let read_back = ok(
            case.label,
            call_in(
                &router,
                case.region,
                "GetDbBackup",
                json!({ "identifier": &id }),
            )
            .await,
        );
        assert_eq!(
            without_status(&read_back),
            without_status(&created),
            "case {:?}: GetDbBackup must answer the stored record the create reported",
            case.label
        );
        assert!(
            LIVE_BACKUP_STATUSES.contains(&read_back["status"].as_str().unwrap_or_default()),
            "case {:?}: a stored backup is being taken or taken, got {read_back}",
            case.label
        );

        // The tags the create attached are the backup's own, reachable through its ARN.
        let arn = string_member(case.label, &created, "arn");
        let listed = ok(
            case.label,
            call_in(
                &router,
                case.region,
                "ListTagsForResource",
                json!({ "resourceArn": &arn }),
            )
            .await,
        );
        let expected_tags: BTreeMap<String, String> = case
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let reported_tags: BTreeMap<String, String> = listed["tags"]
            .as_object()
            .map(|tags| {
                tags.iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported_tags, expected_tags,
            "case {:?}: a backup reports the tags its create attached, got {listed}",
            case.label
        );
    }
}

#[tokio::test]
async fn the_expiry_date_a_backup_reports_follows_the_retention_asked_for() {
    let router = backup_router();
    for case in backup_cases(&router).await {
        let source_id = create_source(&router, &case).await;
        let created = ok(
            case.label,
            call_in(
                &router,
                case.region,
                "CreateDbBackup",
                backup_request(&case, &source_id),
            )
            .await,
        );
        let taken_at = created_at_secs(case.label, &created);
        match case.retention_days {
            None => assert_eq!(
                created.get("expiresAfter"),
                None,
                "case {:?}: a backup with no retention is past no date, got {created}",
                case.label
            ),
            Some(days) => {
                let reported = string_member(case.label, &created, "expiresAfter");
                assert!(
                    is_modelled_date(&reported),
                    "case {:?}: expected a YYYY-MM-DD date, got {reported:?}",
                    case.label
                );
                assert_eq!(
                    reported,
                    date_after(taken_at, days),
                    "case {:?}: expected the date {days} days after the backup was taken, \
                     got {reported:?}",
                    case.label
                );
                assert!(
                    reported > date_after(taken_at, 0),
                    "case {:?}: a retained backup expires after the day it was taken, got \
                     {reported:?}",
                    case.label
                );
            }
        }
    }
}

#[tokio::test]
async fn a_retention_outside_the_published_range_is_refused_and_stores_nothing() {
    let router = backup_router();
    let source = ok(
        "setup",
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("retention-bounds"),
        )
        .await,
    );
    let source_id = string_member("setup", &source, "id");

    // The published bounds of `RetentionDays` are accepted; one past either end is not.
    for (label, days, expected_status) in [
        ("the published lower bound", RETENTION_DAYS_RANGE.0, 200),
        ("the published upper bound", RETENTION_DAYS_RANGE.1, 200),
        ("one below the lower bound", RETENTION_DAYS_RANGE.0 - 1, 400),
        ("one above the upper bound", RETENTION_DAYS_RANGE.1 + 1, 400),
    ] {
        let (status, body) = call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({
                "name": format!("retention{days}"),
                "dbResourceId": &source_id,
                "retentionDays": days,
            }),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {label:?}: expected HTTP {expected_status} for {days} days, got {status} \
             with {body}"
        );
    }
}

#[tokio::test]
async fn a_backup_being_taken_settles_to_completed_on_the_clock() {
    let router = backup_router();
    let source = ok(
        "setup",
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("settling-source"),
        )
        .await,
    );
    let source_id = string_member("setup", &source, "id");
    let created = ok(
        "the backup is taken",
        call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({ "name": "settling-backup", "dbResourceId": &source_id }),
        )
        .await,
    );
    assert_eq!(
        created["status"].as_str(),
        Some("IN_PROGRESS"),
        "case \"the backup is taken\": taking a backup is work, got {created}"
    );
    let id = string_member("the backup is taken", &created, "id");

    // Poll the way a client does: look, and if the backup is still being taken look again
    // one probe interval later, until the generous deadline above passes.
    let started = Instant::now();
    let settled = loop {
        let body = ok(
            "the clock settles the backup",
            call_in(&router, REGION, "GetDbBackup", json!({ "identifier": &id })).await,
        );
        match body["status"].as_str() {
            Some("COMPLETED") => break body,
            Some("IN_PROGRESS") => {}
            other => panic!(
                "case \"the clock settles the backup\": a backup being taken reports \
                 IN_PROGRESS until the clock settles it to COMPLETED, got {other:?}"
            ),
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case \"the clock settles the backup\": the clock-owed settle to COMPLETED \
             never happened, last answer {body}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    };
    assert_eq!(
        settled["createdAt"], created["createdAt"],
        "case \"the clock settles the backup\": the settle changes the status only, got \
         {settled}"
    );
}

#[tokio::test]
async fn a_backup_keeps_what_it_captured_when_its_source_changes() {
    let router = backup_router();
    let source = ok(
        "setup",
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("mutating-source"),
        )
        .await,
    );
    let source_id = string_member("setup", &source, "id");
    let created = ok(
        "the backup is taken",
        call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({ "name": "captured-backup", "dbResourceId": &source_id }),
        )
        .await,
    );
    let id = string_member("the backup is taken", &created, "id");

    // Change every member of the source the backup captured a value for.
    let changes = json!({
        "identifier": &source_id,
        "dbInstanceType": "db.influx.16xlarge",
        "allocatedStorage": 1200,
        "port": 1024,
        "dbStorageType": "InfluxIOIncludedT2",
        "deploymentType": "WITH_MULTIAZ_STANDBY",
        "dbParameterGroupIdentifier": "InfluxDBV3Core",
        "maintenanceSchedule": {
            "timezone": "UTC",
            "preferredMaintenanceWindow": "Mon:04:00-Mon:04:30"
        },
    });
    let updated = ok(
        "the source is changed",
        call_in(&router, REGION, "UpdateDbInstance", changes).await,
    );

    let read_back = ok(
        "the backup after the change",
        call_in(&router, REGION, "GetDbBackup", json!({ "identifier": &id })).await,
    );
    for (backup_member, _) in SNAPSHOT_MEMBERS {
        assert_eq!(
            read_back.get(backup_member),
            created.get(backup_member),
            "case \"the backup after the change\": the backup must keep the {backup_member} \
             it captured, got {:?} against {:?}",
            read_back.get(backup_member),
            created.get(backup_member)
        );
    }
    for member in ["dbInstanceType", "allocatedStorage", "port"] {
        assert_ne!(
            read_back.get(member),
            updated.get(member),
            "case \"the backup after the change\": the source's new {member} must not reach \
             the backup, got {read_back}"
        );
    }
}

#[tokio::test]
async fn a_create_naming_a_db_resource_this_account_holds_none_of_is_refused() {
    let router = backup_router();
    // Ids as the modelled `DbResourceId` pattern allows them: `[a-zA-Z0-9]{3,64}`.
    let unminted = ["zzz", &"a1b2c3".repeat(4), &"9".repeat(64)];
    for (index, resource_id) in unminted.iter().enumerate() {
        let name = format!("orphan-backup-{index}");
        let (status, body) = call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({ "name": &name, "dbResourceId": resource_id }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {resource_id:?}: a backup of a resource nothing holds must be refused, \
             got {status} with {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with("ResourceNotFoundException"),
            "case {resource_id:?}: expected a ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(*resource_id),
            "case {resource_id:?}: the refusal must name the id asked for, got {body}"
        );

        // The refused create stored nothing — not even the name it asked for, which a
        // later create of a real backup can still take.
        let source = ok(
            "the name is still free",
            call_in(
                &router,
                REGION,
                "CreateDbInstance",
                minimal_instance(&format!("orphan-source-{index}")),
            )
            .await,
        );
        let source_id = string_member("the name is still free", &source, "id");
        ok(
            "the name is still free",
            call_in(
                &router,
                REGION,
                "CreateDbBackup",
                json!({ "name": &name, "dbResourceId": &source_id }),
            )
            .await,
        );
    }
}

#[tokio::test]
async fn a_backup_name_is_unique_within_the_account_and_region() {
    let router = backup_router();
    let source = ok(
        "setup",
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("unique-source"),
        )
        .await,
    );
    let source_id = string_member("setup", &source, "id");
    let elsewhere = ok(
        "setup: a source in the other Region",
        call_in(
            &router,
            OTHER_REGION,
            "CreateDbInstance",
            minimal_instance("unique-source"),
        )
        .await,
    );
    let elsewhere_id = string_member("setup: a source in the other Region", &elsewhere, "id");

    for name in ["abc", "taken-name", "a23456789-123456789-123456789-123456789"] {
        let first = ok(
            name,
            call_in(
                &router,
                REGION,
                "CreateDbBackup",
                json!({ "name": name, "dbResourceId": &source_id }),
            )
            .await,
        );
        let first_id = string_member(name, &first, "id");

        let (status, refusal) = call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({ "name": name, "dbResourceId": &source_id, "retentionDays": 5 }),
        )
        .await;
        assert_eq!(
            status, 409,
            "case {name:?}: a second backup under a stored name must conflict, got {status} \
             with {refusal}"
        );
        assert_eq!(
            refusal["resourceId"].as_str(),
            Some(first_id.as_str()),
            "case {name:?}: the refusal must name the backup that holds the name, got \
             {refusal}"
        );
        assert_eq!(
            refusal["resourceType"].as_str(),
            Some("DB_BACKUP"),
            "case {name:?}: the refusal must name the resource kind, got {refusal}"
        );

        // The same name in another Region is a new backup: the index is per Region.
        let other = ok(
            name,
            call_in(
                &router,
                OTHER_REGION,
                "CreateDbBackup",
                json!({ "name": name, "dbResourceId": &elsewhere_id }),
            )
            .await,
        );
        let other_id = string_member(name, &other, "id");
        assert_ne!(
            other_id, first_id,
            "case {name:?}: the same name in another Region is another backup"
        );
        assert!(
            other["arn"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!(":{OTHER_REGION}:")),
            "case {name:?}: the backup's ARN is scoped to the Region it was taken in, got \
             {other}"
        );
    }
}

#[tokio::test]
async fn get_db_backup_refuses_an_id_nothing_minted() {
    let router = backup_router();
    let source = ok(
        "setup",
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("reader-source"),
        )
        .await,
    );
    let source_id = string_member("setup", &source, "id");
    let created = ok(
        "setup: a real backup",
        call_in(
            &router,
            REGION,
            "CreateDbBackup",
            json!({ "name": "reader-backup", "dbResourceId": &source_id }),
        )
        .await,
    );
    let backup_id = string_member("setup: a real backup", &created, "id");

    // Rows: an id nothing minted, the id of a resource of another family, and the backup's
    // own name where its id belongs.
    for (label, identifier) in [
        ("an id nothing minted", "zzzzzzzzzz".to_string()),
        ("the source instance's id", source_id.clone()),
        ("the backup's name", "readerbackup".to_string()),
    ] {
        let (status, body) = call_in(
            &router,
            REGION,
            "GetDbBackup",
            json!({ "identifier": &identifier }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected HTTP 404 for {identifier:?}, got {status} with {body}"
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(identifier.as_str()),
            "case {label:?}: the refusal must name the identifier asked for, got {body}"
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some("DB_BACKUP"),
            "case {label:?}: the refusal must name the resource kind, got {body}"
        );
    }

    // The real backup is still readable, so the refusals above wrote nothing.
    let read_back = ok(
        "the real backup",
        call_in(
            &router,
            REGION,
            "GetDbBackup",
            json!({ "identifier": &backup_id }),
        )
        .await,
    );
    assert_eq!(
        read_back["id"].as_str(),
        Some(backup_id.as_str()),
        "case \"the real backup\": expected the stored record, got {read_back}"
    );
}

#[test]
fn every_backup_status_and_type_this_service_reports_is_a_modelled_value() {
    use influxdb::ops::types::shared_types::{DB_BACKUP_STATUS_VALUES, DB_BACKUP_TYPE_VALUES};
    use influxdb::state as st;

    for status in [
        st::DB_BACKUP_STATUS_IN_PROGRESS,
        st::DB_BACKUP_STATUS_COMPLETED,
    ] {
        assert!(
            DB_BACKUP_STATUS_VALUES.contains(&status),
            "case {status:?}: every backup status this service reports must be a member of \
             the modelled DbBackupStatus enum {DB_BACKUP_STATUS_VALUES:?}"
        );
    }
    assert!(
        DB_BACKUP_TYPE_VALUES.contains(&st::DB_BACKUP_TYPE_ON_DEMAND),
        "case {:?}: the backup type this service reports must be a member of the modelled \
         DbBackupType enum {DB_BACKUP_TYPE_VALUES:?}",
        st::DB_BACKUP_TYPE_ON_DEMAND
    );
    for status in LIVE_BACKUP_STATUSES {
        assert!(
            DB_BACKUP_STATUS_VALUES.contains(status),
            "case {status:?}: every status this suite waits on must be a member of the \
             modelled DbBackupStatus enum {DB_BACKUP_STATUS_VALUES:?}"
        );
    }
}

// `ListDbBackups`: the paginated REPORT of the stored backup records.
//
// RULE: a listing reports the backups the account holds in the Region the call is made in,
// each as the summary projection of its own stored record, narrowed to the DB instance or
// DB cluster `dbResourceId` names when the request names one; it yields every such record
// exactly once across pages of at most `maxResults` items, and answers a `nextToken` only
// while a further page remains.

/// Every member of `DbBackupSummary`, from
/// `smithy_query(query="shape", shape="DbBackupSummary")`. A listed summary must report
/// the stored record's own value for each one, so each is checked against `GetDbBackup`.
const BACKUP_SUMMARY_MEMBERS: &[&str] = &[
    "arn",
    "createdAt",
    "dbResourceId",
    "deploymentType",
    "engineType",
    "expiresAfter",
    "id",
    "kmsKeyId",
    "name",
    "status",
    "type",
];

/// The `DbBackupSummary` members every stored backup reports: the two the model marks
/// `@required` (`id`, `arn`) plus the ones this service always stores a value for.
const ALWAYS_REPORTED_BACKUP_SUMMARY_MEMBERS: &[&str] = &[
    "arn",
    "createdAt",
    "dbResourceId",
    "engineType",
    "id",
    "name",
    "status",
    "type",
];

/// A generous bound on the pages a listing of this suite's backups takes: a paging loop
/// that never terminates fails here instead of running until the harness gives up.
const MAX_PAGES: usize = 64;

/// An id the modelled `DbResourceId` pattern allows (`^[a-zA-Z0-9]+$`, length 3..64) that
/// nothing in the store minted, so no stored backup was taken from it.
const UNMINTED_RESOURCE_ID: &str = "zzz";

/// One DB instance and one DB cluster to take backups of, created in [`REGION`], so a
/// listing spans a backup of each source family.
async fn two_sources(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let instance = ok(
        case,
        call_in(
            router,
            REGION,
            "CreateDbInstance",
            minimal_instance("listed-source"),
        )
        .await,
    );
    let cluster = ok(
        case,
        call_in(
            router,
            REGION,
            "CreateDbCluster",
            minimal_cluster("listed-cluster"),
        )
        .await,
    );
    vec![
        string_member(case, &instance, "id"),
        string_member(case, &cluster, "dbClusterId"),
    ]
}

/// Drain every page of `ListDbBackups` for `request`, the way a paginating client does:
/// each answer's `nextToken` resumes the next call, and no page carries more items than
/// the `maxResults` asked for. Hands back the items in page order.
async fn listed_backups(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    request: &Value,
    max_results: Option<i64>,
) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(
            pages <= MAX_PAGES,
            "case {case:?}: paging did not terminate after {pages} pages, saw {} items",
            items.len()
        );
        let mut body = request.clone();
        if let Some(limit) = max_results {
            body["maxResults"] = json!(limit);
        }
        if let Some(token) = &token {
            body["nextToken"] = json!(token);
        }
        let page = ok(case, call_in(router, region, "ListDbBackups", body).await);
        let listed = page["items"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected an `items` list, got {page}"))
            .clone();
        if let Some(limit) = max_results {
            assert!(
                listed.len() as i64 <= limit,
                "case {case:?}: a page of {} items exceeds maxResults {limit}",
                listed.len()
            );
        }
        items.extend(listed);
        match page.get("nextToken").and_then(Value::as_str) {
            Some(next) => {
                assert!(
                    page["items"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty()),
                    "case {case:?}: an empty page must not hand back a nextToken: {page}"
                );
                token = Some(next.to_string());
            }
            None => break,
        }
    }
    items
}

/// The `id` of every item of a listing.
fn listed_ids(case: &str, listed: &[Value]) -> Vec<String> {
    listed
        .iter()
        .map(|item| string_member(case, item, "id"))
        .collect()
}

#[tokio::test]
async fn paging_yields_every_stored_backup_exactly_once() {
    // (backups taken, maxResults): `MaxResults` is modelled range 1..100, and an omitted
    // one pages at that maximum. The backups alternate between the two source families,
    // so a page spans a backup of each.
    let cases: [(usize, Option<i64>); 6] = [
        (0, None),
        (1, None),
        (2, Some(1)),
        (5, Some(2)),
        (3, Some(100)),
        (7, Some(3)),
    ];
    for (count, max_results) in cases {
        let case = format!("{count} backups at maxResults {max_results:?}");
        let router = backup_router();
        let sources = two_sources(&router, &case).await;
        let mut expected: Vec<String> = Vec::new();
        for index in 0..count {
            let created = ok(
                &case,
                call_in(
                    &router,
                    REGION,
                    "CreateDbBackup",
                    json!({
                        "name": format!("paged-{index}"),
                        "dbResourceId": &sources[index % sources.len()],
                    }),
                )
                .await,
            );
            expected.push(string_member(&case, &created, "id"));
        }
        expected.sort();

        let listed = listed_backups(&router, REGION, &case, &json!({}), max_results).await;
        for item in &listed {
            for member in ALWAYS_REPORTED_BACKUP_SUMMARY_MEMBERS {
                assert!(
                    item.get(member).is_some(),
                    "case {case:?}: the summary omits {member}: {item}"
                );
            }
        }
        let mut seen = listed_ids(&case, &listed);
        let yielded = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(
            yielded,
            seen.len(),
            "case {case:?}: paging repeated a backup, got {listed:?}"
        );
        assert_eq!(
            seen, expected,
            "case {case:?}: paging must yield exactly the stored backups, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_narrows_to_the_db_resource_it_names() {
    let setup = "backups of two sources";
    let router = backup_router();
    let sources = two_sources(&router, setup).await;
    // Three backups of the first source and two of the second, so a filter that reported
    // the whole store, or only one record, would differ from the expected set.
    let mut taken: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (index, which) in [0usize, 0, 0, 1, 1].into_iter().enumerate() {
        let resource = sources[which].clone();
        let created = ok(
            setup,
            call_in(
                &router,
                REGION,
                "CreateDbBackup",
                json!({
                    "name": format!("filtered-{index}"),
                    "dbResourceId": &resource,
                }),
            )
            .await,
        );
        taken
            .entry(resource)
            .or_default()
            .push(string_member(setup, &created, "id"));
    }
    let mut every_backup: Vec<String> = taken.values().flatten().cloned().collect();
    every_backup.sort();

    // (what the request asks for, the backups it must report).
    let mut rows: Vec<(String, Option<String>, Vec<String>)> =
        vec![("no filter at all".to_string(), None, every_backup)];
    for (resource, ids) in &taken {
        let mut ids = ids.clone();
        ids.sort();
        rows.push((
            format!("the backups of source {resource}"),
            Some(resource.clone()),
            ids,
        ));
    }
    rows.push((
        "a DB resource nothing was backed up from".to_string(),
        Some(UNMINTED_RESOURCE_ID.to_string()),
        Vec::new(),
    ));

    for (label, filter, expected) in rows {
        // The filter holds page by page as well as in one answer.
        for max_results in [None, Some(1), Some(2)] {
            let case = format!("{label} at maxResults {max_results:?}");
            let mut request = json!({});
            if let Some(filter) = &filter {
                request["dbResourceId"] = json!(filter);
            }
            let listed = listed_backups(&router, REGION, &case, &request, max_results).await;
            let mut seen = listed_ids(&case, &listed);
            seen.sort();
            assert_eq!(
                seen, expected,
                "case {case:?}: expected {expected:?}, got {listed:?}"
            );
            if let Some(filter) = &filter {
                for item in &listed {
                    assert_eq!(
                        item["dbResourceId"].as_str(),
                        Some(filter.as_str()),
                        "case {case:?}: every reported backup must be one of that \
                         resource's, got {item}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn every_listed_summary_agrees_with_the_stored_record() {
    let case = "listed summaries";
    let router = backup_router();
    // A source carrying a KMS key beside one that carries none, so the summary member
    // that is reported only for an encrypted resource is covered either way.
    let mut encrypted = minimal_instance("summary-source");
    encrypted["kmsKeyId"] = json!(format!(
        "arn:aws:kms:{REGION}:{ACCOUNT}:key/0123456789abcdef"
    ));
    let instance = ok(
        case,
        call_in(&router, REGION, "CreateDbInstance", encrypted).await,
    );
    let plain = ok(
        case,
        call_in(
            &router,
            REGION,
            "CreateDbInstance",
            minimal_instance("summary-plain"),
        )
        .await,
    );
    let cluster = ok(
        case,
        call_in(
            &router,
            REGION,
            "CreateDbCluster",
            minimal_cluster("summary-cluster"),
        )
        .await,
    );
    // (the backup's name, the resource it is taken from, the retention asked for): the
    // rows span the source family, whether the source is encrypted, and the retention at
    // both published bounds of the modelled `RetentionDays` range and absent.
    let rows = [
        (
            "summary-encrypted",
            string_member(case, &instance, "id"),
            Some(RETENTION_DAYS_RANGE.0),
        ),
        (
            "summary-plain",
            string_member(case, &plain, "id"),
            None,
        ),
        (
            "summary-cluster",
            string_member(case, &cluster, "dbClusterId"),
            Some(RETENTION_DAYS_RANGE.1),
        ),
    ];
    for (name, resource, retention) in &rows {
        let mut body = json!({ "name": name, "dbResourceId": resource });
        if let Some(days) = retention {
            body["retentionDays"] = json!(days);
        }
        ok(case, call_in(&router, REGION, "CreateDbBackup", body).await);
    }

    let listed = listed_backups(&router, REGION, case, &json!({}), None).await;
    assert_eq!(
        listed.len(),
        rows.len(),
        "case {case:?}: expected one summary per stored backup, got {listed:?}"
    );
    for item in &listed {
        let id = string_member(case, item, "id");
        assert!(
            is_modelled_db_backup_id(&id),
            "case {case:?}: a listed id must satisfy the modelled DbBackupId constraint, \
             got {id:?}"
        );
        let record = ok(
            case,
            call_in(&router, REGION, "GetDbBackup", json!({ "identifier": &id })).await,
        );
        for member in BACKUP_SUMMARY_MEMBERS {
            // The status excepted: the CLOCK settles it, so two reads of one record may
            // straddle that settle. It is asserted on its own below.
            if *member == "status" {
                continue;
            }
            assert_eq!(
                item.get(member),
                record.get(member),
                "case {case:?} member {member:?}: the summary must report the stored \
                 record's value, got {:?} against {:?}",
                item.get(member),
                record.get(member)
            );
        }
        let status = item["status"].as_str().unwrap_or_default();
        assert!(
            LIVE_BACKUP_STATUSES.contains(&status),
            "case {case:?}: a listed backup reports a live status, got {status:?}"
        );
    }

    // Poll the way a client does: look again one probe interval later until every backup
    // the store holds reports the status the clock owed it, or the generous deadline
    // above passes. A listing reads the same records the reader does, so the settle shows
    // in the listing too.
    let started = Instant::now();
    loop {
        let listed = listed_backups(&router, REGION, case, &json!({}), None).await;
        let statuses: Vec<&str> = listed
            .iter()
            .map(|item| item["status"].as_str().unwrap_or_default())
            .collect();
        if statuses.iter().all(|status| *status == "COMPLETED") {
            break;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the clock-owed settle to COMPLETED never reached the \
             listing, last statuses {statuses:?}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_listing_reports_only_the_backups_of_the_region_it_is_made_in() {
    let setup = "a backup in each of two Regions";
    let router = backup_router();
    let mut expected: BTreeMap<&str, String> = BTreeMap::new();
    for region in [REGION, OTHER_REGION] {
        let source = ok(
            setup,
            call_in(
                &router,
                region,
                "CreateDbInstance",
                minimal_instance("regional-source"),
            )
            .await,
        );
        let source_id = string_member(setup, &source, "id");
        let created = ok(
            setup,
            call_in(
                &router,
                region,
                "CreateDbBackup",
                json!({ "name": "regional-backup", "dbResourceId": &source_id }),
            )
            .await,
        );
        expected.insert(region, string_member(setup, &created, "id"));
    }

    for region in [REGION, OTHER_REGION] {
        let case = format!("a listing in {region}");
        let listed = listed_backups(&router, region, &case, &json!({}), None).await;
        assert_eq!(
            listed_ids(&case, &listed),
            vec![expected[region].clone()],
            "case {case:?}: a listing reports the backups of its own Region only, got \
             {listed:?}"
        );
    }
}
