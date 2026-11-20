// SPDX-License-Identifier: CC0-1.0

//! The create -> AVAILABLE -> delete -> identical re-create round trip, driven the way a
//! client drives it, over the same `awsJson1_0` router the production router dispatches
//! with (the harness in [`crate::db_instance_lifecycle`]).
//!
//! RULE, in two halves, both about LIVENESS of this service's DB-instance lifecycle:
//!
//! * Every transient status this service reports is settled by the CLOCK, and the settle
//!   it owes falls due one transition span after the call that started it — a span shorter
//!   than the round trip a client needs to issue its next read. So a client that polls
//!   `GetDbInstance` observes the settled status on its first probe after the call, and a
//!   workflow that waits through several transitions never spends one of its probe
//!   intervals per transition.
//! * A name a live DB instance owns is a STATE, not an always-invalid input: while the
//!   instance lives a second create under that name is refused `ConflictException`, and
//!   `DeleteDbInstance` releases both the id and the name AT ONCE, so the identical
//!   create is accepted again straight after the delete and mints a NEW id.
//!
//! Each row drives the whole round trip twice over (an instance, its replacement, and the
//! replacement's own replacement), so the rule is asserted on a name that has already been
//! taken and released rather than only on a fresh one. The rows span what the rule
//! generalises over: the Region, the name's shape inside the modelled `DbInstanceName`
//! pattern, the deployment type (so one row carries a standby in a second Availability
//! Zone), the storage type, the port and how many optional members the record holds.
//!
//! Every status named here is a member of the modelled `Status` enum
//! (`smithy_query(query="shape", shape="Status")`); the refusal shapes are the ones
//! `CreateDbInstance`, `GetDbInstance` and `DeleteDbInstance` declare
//! (`smithy_query(query="operation", operation="CreateDbInstance,GetDbInstance,DeleteDbInstance")`).
//!
//! The second half of this file is about `RestoreFromDbBackup`, the operation that turns a
//! DB backup back into a resource; its own rule is stated on
//! [`a_restore_builds_the_resource_the_backup_captured`].

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_timestreaminfluxdb_amazontimestreaminfluxdb as influxdb;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_cluster_marketplace_subscription::assert_missing_subscription_refusal;
use crate::db_instance_lifecycle::{call_in, ok, router, router_and_engine, ACCOUNT};
use crate::influx_data_plane_round_trip::{address, basic, exchange, poll_health, stored_points};

/// How long this suite waits for a clock-owed settle before giving up: far above the span
/// the service settles a transient status after, so a loaded host cannot fail a test that
/// is about the settle happening at all.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a polling client waits before looking again. Longer than the transition span,
/// which is what the rule's first half is about: a probe that lands one interval after the
/// call finds the settle already due.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// The most probes a client may need to see a transient status settled: the one that
/// lands while the span is still running, and the next one. A client that needs more than
/// this is burning more than one of its probe intervals on a transition the clock should
/// already have made due. (A client of THIS harness reaches the service in microseconds,
/// far inside the span, so its first probe is the one that lands early; a client that
/// reaches it over a network sees the settle on its first probe.)
const MAX_PROBES: usize = 2;

/// One row of the table: a create request, the Region it is made in, and the status the
/// record must reach.
struct RoundTrip {
    label: &'static str,
    region: &'static str,
    /// The `name` every create of this row asks for — the name whose ownership moves from
    /// one instance to its replacement.
    name: &'static str,
    /// The request body, built from [`Self::name`] by the row's own builder.
    body: fn(&str) -> Value,
}

/// The minimum a `CreateDbInstance` request must carry, per the model's `@required`
/// members (`name`, `password`, `dbInstanceType`, `vpcSubnetIds`, `vpcSecurityGroupIds`,
/// `allocatedStorage`).
fn minimal_create(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "allocatedStorage": 20,
        "vpcSubnetIds": ["subnet-aaa"],
        "vpcSecurityGroupIds": ["sg-aaa"],
    })
}

/// A create carrying a Multi-AZ standby, two subnets and the third storage type.
fn multi_az_create(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret2",
        "dbInstanceType": "db.influx.large",
        "allocatedStorage": 400,
        "dbStorageType": "InfluxIOIncludedT3",
        "deploymentType": "WITH_MULTIAZ_STANDBY",
        "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
        "vpcSecurityGroupIds": ["sg-aaa", "sg-bbb"],
        "port": 1024,
        "networkType": "DUAL",
        "publiclyAccessible": true,
        "username": "admin",
        "organization": "sim2real",
        "bucket": "metrics",
        "tags": { "env": "test", "owner": "round-trip" },
    })
}

/// A create of a single-AZ instance at the top of the modelled `allocatedStorage` range.
fn large_single_az_create(name: &str) -> Value {
    json!({
        "name": name,
        "password": "supersecret3",
        "dbInstanceType": "db.influx.24xlarge",
        "allocatedStorage": 15360,
        "dbStorageType": "InfluxIOIncludedT2",
        "deploymentType": "SINGLE_AZ",
        "vpcSubnetIds": ["subnet-ccc", "subnet-ddd", "subnet-eee"],
        "vpcSecurityGroupIds": ["sg-ccc"],
        "port": 65535,
        "networkType": "IPV4",
    })
}

/// Rows spanning what the rule generalises over.
fn round_trips() -> Vec<RoundTrip> {
    vec![
        RoundTrip {
            label: "only the required members",
            region: "us-west-2",
            // The shortest name the modelled `DbInstanceName` length (3..40) allows.
            name: "abc",
            body: minimal_create,
        },
        RoundTrip {
            label: "a Multi-AZ standby with every optional member set",
            region: "eu-central-1",
            name: "Restored-Instance-1",
            body: multi_az_create,
        },
        RoundTrip {
            label: "a single-AZ instance at the top of the storage range",
            region: "ap-southeast-4",
            // The longest name the modelled `DbInstanceName` length (3..40) allows.
            name: "a-bcdefghijklmnopqrstuvwxyz-0123456789",
            body: large_single_az_create,
        },
    ]
}

/// The `id` of a DB instance record in a response body.
fn id_of(case: &str, body: &Value) -> String {
    body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string `id`, got {body}"))
        .to_string()
}

/// Read the record back until it reports `AVAILABLE`, the way a client polls: look, and if
/// the status is still the `transient` one the call announced, look again one probe
/// interval later, until the generous deadline above passes. Hands back the settled record
/// and how many probes it took.
async fn poll_to_available(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
    case: &str,
    transient: &str,
) -> (Value, usize) {
    let started = Instant::now();
    let mut probes = 0;
    loop {
        probes += 1;
        let body = ok(
            case,
            call_in(
                router,
                region,
                "GetDbInstance",
                json!({ "identifier": identifier }),
            )
            .await,
        );
        let status = body["status"].as_str().unwrap_or_default();
        if status == "AVAILABLE" {
            return (body, probes);
        }
        assert_eq!(
            status, transient,
            "case {case:?}: an instance coming up must report {transient} until the clock \
             settles it, got {body}"
        );
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the clock-owed settle to AVAILABLE never happened, last \
             answer {body}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Every instance in `region` that owns `name`, by id.
///
/// `DbInstanceSummary` carries no Region member, so the Region a listed instance belongs
/// to is read out of its `arn` — whose Region segment is the one the create was made in.
async fn owners_of_name(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    case: &str,
) -> Vec<String> {
    let listed = ok(
        case,
        call_in(router, region, "ListDbInstances", json!({})).await,
    );
    let scope = format!(":{region}:");
    listed["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an `items` list, got {listed}"))
        .iter()
        .filter(|item| item["name"].as_str() == Some(name))
        .filter(|item| item["arn"].as_str().unwrap_or_default().contains(&scope))
        .map(|item| id_of(case, item))
        .collect()
}

/// Create `name`, assert the whole create leg (the transient status, the conflict a live
/// name refuses, the single owner the listing reports) and poll the instance to AVAILABLE.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &RoundTrip,
    round: usize,
    previous: Option<&str>,
) -> String {
    let label = format!("{} (round {round})", case.label);
    let created = ok(
        &label,
        call_in(router, case.region, "CreateDbInstance", (case.body)(case.name)).await,
    );
    let id = id_of(&label, &created);
    assert_eq!(
        created["status"].as_str(),
        Some("CREATING"),
        "case {label:?}: a freshly created instance is provisioning, got {created}"
    );
    assert_eq!(
        created["name"].as_str(),
        Some(case.name),
        "case {label:?}: the record must carry the name asked for, got {created}"
    );
    if let Some(previous) = previous {
        assert_ne!(
            id, previous,
            "case {label:?}: the replacement instance must be a newly minted id"
        );
    }

    // While this instance lives the name is taken: the identical create is refused, and
    // the refusal names the record that HOLDS the name.
    let mut duplicate = (case.body)(case.name);
    duplicate["password"] = json!("anotherpassword9");
    let (status, refusal) = call_in(router, case.region, "CreateDbInstance", duplicate).await;
    assert_eq!(
        status, 409,
        "case {label:?}: a second create under a live name must conflict, got {status} \
         with {refusal}"
    );
    assert!(
        refusal["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("ConflictException"),
        "case {label:?}: expected a ConflictException, got {refusal}"
    );
    assert_eq!(
        refusal["resourceId"].as_str(),
        Some(id.as_str()),
        "case {label:?}: the conflict must name the instance that holds the name, got \
         {refusal}"
    );
    assert_eq!(
        refusal["message"].as_str(),
        Some(
            format!(
                "The DbInstance with id [{id}], name [{}], customer [{ACCOUNT}] already \
                 exists",
                case.name
            )
            .as_str()
        ),
        "case {label:?}: unexpected conflict message in {refusal}"
    );

    // The refused create left the state alone: exactly one instance owns the name.
    let owners = owners_of_name(router, case.region, case.name, &label).await;
    assert_eq!(
        owners,
        vec![id.clone()],
        "case {label:?}: exactly the created instance must own the name"
    );

    // The settle the create owes is due before this client's next probe, so the poll
    // converges without burning an interval on every transition.
    let (settled, probes) = poll_to_available(router, case.region, &id, &label, "CREATING").await;
    assert_eq!(
        settled["id"].as_str(),
        Some(id.as_str()),
        "case {label:?}: the settled record must be the one created, got {settled}"
    );
    assert!(
        probes <= MAX_PROBES,
        "case {label:?}: a client needed {probes} probes to see the instance settled, \
         expected at most {MAX_PROBES}"
    );
    id
}

/// Delete `id` and assert the delete leg: the final record, the id resolving nowhere and
/// the name free again — all of it with no polling at all, because the delete releases
/// both AT ONCE.
async fn delete_and_release(router: &MultiServiceRouter, case: &RoundTrip, round: usize, id: &str) {
    let label = format!("{} (round {round})", case.label);
    let deleted = ok(
        &label,
        call_in(
            router,
            case.region,
            "DeleteDbInstance",
            json!({ "identifier": id }),
        )
        .await,
    );
    assert_eq!(
        deleted["status"].as_str(),
        Some("DELETING"),
        "case {label:?}: the final record must report DELETING, got {deleted}"
    );
    assert_eq!(
        deleted["id"].as_str(),
        Some(id),
        "case {label:?}: the final record must be the one asked for, got {deleted}"
    );

    let (status, body) = call_in(
        router,
        case.region,
        "GetDbInstance",
        json!({ "identifier": id }),
    )
    .await;
    assert_eq!(
        status, 404,
        "case {label:?}: a deleted id must stop resolving at once, got {status} with {body}"
    );
    assert!(
        body["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("ResourceNotFoundException"),
        "case {label:?}: expected a ResourceNotFoundException, got {body}"
    );

    let owners = owners_of_name(router, case.region, case.name, &label).await;
    assert!(
        owners.is_empty(),
        "case {label:?}: the deleted instance must own the name no longer, got {owners:?}"
    );
}

#[tokio::test]
async fn the_name_of_a_deleted_instance_is_free_for_an_identical_create_at_once() {
    for case in round_trips() {
        // One router per row, so the listing assertions are about this row's own state.
        let router = router();
        let mut previous: Option<String> = None;
        // Three rounds: a fresh name, the name of a deleted instance, and that name once
        // more — a name that has been taken and released twice.
        for round in 1..=3 {
            let id = create_and_settle(&router, &case, round, previous.as_deref()).await;
            delete_and_release(&router, &case, round, &id).await;
            previous = Some(id);
        }
    }
}

#[tokio::test]
async fn a_restored_instance_is_a_resource_of_its_own() {
    // The round trip across TWO Regions at once: the same name is restored in each, and
    // neither restore disturbs the other.
    let router = router();
    let regions = ["us-west-2", "eu-central-1"];
    let name = "shared-restored-name";
    let case = |region: &'static str| RoundTrip {
        label: "a name restored in two Regions",
        region,
        name,
        body: minimal_create,
    };

    let mut first = Vec::new();
    for region in regions {
        first.push(create_and_settle(&router, &case(region), 1, None).await);
    }
    for (region, id) in regions.iter().zip(&first) {
        delete_and_release(&router, &case(region), 1, id).await;
    }

    let mut second = Vec::new();
    for (region, previous) in regions.iter().zip(&first) {
        second.push(create_and_settle(&router, &case(region), 2, Some(previous)).await);
    }
    for (region, id) in regions.iter().zip(&second) {
        // Each Region's restored instance is the only owner of the name THERE, and the
        // record it resolves to is scoped to the Region that minted it.
        let owners = owners_of_name(&router, region, name, "two Regions").await;
        assert_eq!(
            owners,
            vec![id.clone()],
            "case \"two Regions\": {region} must report exactly its own restored instance"
        );
        let restored = ok(
            "two Regions",
            call_in(&router, region, "GetDbInstance", json!({ "identifier": id })).await,
        );
        assert!(
            restored["arn"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!(":{region}:")),
            "case \"two Regions\": the restored record must stay scoped to {region}, got \
             {restored}"
        );
        assert_eq!(
            restored["status"].as_str(),
            Some("AVAILABLE"),
            "case \"two Regions\": the restored instance must stay settled, got {restored}"
        );
    }
    assert_ne!(
        second[0], second[1],
        "case \"two Regions\": each Region's restored instance is its own resource, got \
         {second:?}"
    );
}

// =======================================================================================
// `RestoreFromDbBackup`: the backup becomes a resource again.
// =======================================================================================

/// The line protocol a case writes to its source resource BEFORE taking the backup: the
/// data the backup therefore captures.
const POINT_BEFORE_BACKUP: &str = "cpu,host=one value=42i 1700000000000000000";

/// And a point written AFTER it, which the backup does not hold — so a restore that brings
/// the captured data back is told apart from one that brings the source's current data.
const POINT_AFTER_BACKUP: &str = "cpu,host=one value=43i 1700000001000000000";

/// [`POINT_BEFORE_BACKUP`] as the store holds it: `(measurement, first field value,
/// timestamp in nanoseconds)`.
fn point_before_backup() -> (String, String, i64) {
    ("cpu".to_string(), "42".to_string(), 1_700_000_000_000_000_000)
}

/// [`POINT_AFTER_BACKUP`] as the store holds it.
fn point_after_backup() -> (String, String, i64) {
    ("cpu".to_string(), "43".to_string(), 1_700_000_001_000_000_000)
}

/// How a case's source resource is created: the request body, and the InfluxDB admin user,
/// organization and bucket that create gives the instance's own data plane.
struct SourceSpec {
    body: Value,
    username: &'static str,
    password: &'static str,
    organization: &'static str,
    bucket: &'static str,
}

/// A source created with only the required members: its plane holds InfluxDB's own
/// initial-setup defaults, the ones this emulator applies when the create names none.
fn minimal_source(name: &str) -> SourceSpec {
    use influxdb::state::influx_data_plane::time_series::{
        DEFAULT_BUCKET, DEFAULT_ORGANIZATION, DEFAULT_USERNAME,
    };
    SourceSpec {
        body: minimal_create(name),
        username: DEFAULT_USERNAME,
        password: "supersecret1",
        organization: DEFAULT_ORGANIZATION,
        bucket: DEFAULT_BUCKET,
    }
}

/// A source created with every optional member set, including its own admin user,
/// organization and bucket.
fn multi_az_source(name: &str) -> SourceSpec {
    SourceSpec {
        body: multi_az_create(name),
        username: "admin",
        password: "supersecret2",
        organization: "sim2real",
        bucket: "metrics",
    }
}

/// A created source resource: the id the control plane minted, the address it publishes,
/// and the InfluxDB admin user and bucket it was created with.
struct Source {
    id: String,
    host: String,
    port: i64,
    username: String,
    password: String,
    organization: String,
    bucket: String,
}

impl Source {
    /// Write `line` to this resource's own InfluxDB address, the way a client does.
    async fn write(&self, case: &str, line: &str) {
        let target = format!(
            "/api/v2/write?org={}&bucket={}&precision=ns",
            self.organization, self.bucket
        );
        let answer = exchange(
            case,
            &self.host,
            self.port,
            "POST",
            &target,
            &[("authorization", basic(&self.username, &self.password))],
            line,
        )
        .await;
        assert_eq!(
            answer.status(),
            204,
            "case {case:?}: the source instance must take the write, got {} with {:?}",
            answer.status(),
            answer.body()
        );
    }
}

/// Create the DB instance a restore case takes its backup of, settle it, and hand back
/// what a client needs to reach it.
async fn create_source(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    spec: &SourceSpec,
) -> Source {
    let created = ok(
        case,
        call_in(router, region, "CreateDbInstance", spec.body.clone()).await,
    );
    let id = id_of(case, &created);
    let (settled, _) = poll_to_available(router, region, &id, case, "CREATING").await;
    let (host, port) = address(case, &settled);
    poll_health(case, &host, port).await;
    Source {
        id,
        host,
        port,
        username: spec.username.to_string(),
        password: spec.password.to_string(),
        organization: spec.organization.to_string(),
        bucket: spec.bucket.to_string(),
    }
}

/// Take a backup of `db_resource_id` under `name`, and hand back the backup's own id.
async fn take_backup(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    name: &str,
    db_resource_id: &str,
) -> String {
    let taken = ok(
        case,
        call_in(
            router,
            region,
            "CreateDbBackup",
            json!({ "name": name, "dbResourceId": db_resource_id, "retentionDays": 7 }),
        )
        .await,
    );
    taken["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a backup `id`, got {taken}"))
        .to_string()
}

/// A `RestoreFromDbBackup` request: the two members the model marks `@required`, plus
/// whatever optional members the case asks for.
fn restore_request(name: &str, db_backup_id: &str, optional: &Value) -> Value {
    let mut body = json!({ "name": name, "dbBackupId": db_backup_id });
    if let Some(members) = optional.as_object() {
        for (member, value) in members {
            body[member.as_str()] = value.clone();
        }
    }
    body
}

/// One restore case: the source resource, the restore request's optional members, and what
/// the restored DB instance must then report.
struct RestoreCase {
    label: &'static str,
    region: &'static str,
    /// The source resource's create, built from the name the case gives it.
    source: fn(&str) -> SourceSpec,
    /// What the restore request carries beyond `name` and `dbBackupId`.
    request: Value,
    /// `(member, value)` pairs the restored record must report, `null` for a member it must
    /// not carry at all.
    expected: Vec<(&'static str, Value)>,
    /// The whole tag set the restored resource must carry.
    expected_tags: Value,
    /// Whether the restored record must report a standby's Availability Zone, which only
    /// the Multi-AZ deployment type places.
    expects_secondary_zone: bool,
}

/// Rows spanning what the rule generalises over: the Region, how many of the source's
/// optional members the backup captured, and which members the restore request overrides —
/// none of them, or the placement, the network type, the deployment type, the key and the
/// tags at once.
fn restore_cases() -> Vec<RestoreCase> {
    vec![
        RestoreCase {
            label: "a backup of a source created with only the required members",
            region: "us-west-2",
            source: minimal_source,
            request: json!({}),
            expected: vec![
                ("dbInstanceType", json!("db.influx.medium")),
                ("allocatedStorage", json!(20)),
                // The source named no deployment type, so it is a standalone single-AZ
                // resource and the restore reports that.
                ("deploymentType", json!("SINGLE_AZ")),
                ("vpcSubnetIds", json!(["subnet-aaa"])),
                ("vpcSecurityGroupIds", json!(["sg-aaa"])),
                ("dbStorageType", json!(null)),
                ("networkType", json!(null)),
                ("publiclyAccessible", json!(null)),
                ("kmsKeyId", json!(null)),
            ],
            // The restore request named no tag, and a backup captures none of the source's:
            // the tags of a restored resource are the ones the restore asked for.
            expected_tags: json!({}),
            expects_secondary_zone: false,
        },
        RestoreCase {
            label: "a backup of a Multi-AZ source with every optional member set",
            region: "eu-central-1",
            source: multi_az_source,
            request: json!({}),
            expected: vec![
                ("dbInstanceType", json!("db.influx.large")),
                ("allocatedStorage", json!(400)),
                ("dbStorageType", json!("InfluxIOIncludedT3")),
                ("deploymentType", json!("WITH_MULTIAZ_STANDBY")),
                ("networkType", json!("DUAL")),
                ("publiclyAccessible", json!(true)),
                ("vpcSubnetIds", json!(["subnet-aaa", "subnet-bbb"])),
                ("vpcSecurityGroupIds", json!(["sg-aaa", "sg-bbb"])),
            ],
            expected_tags: json!({}),
            expects_secondary_zone: true,
        },
        RestoreCase {
            label: "a restore overriding the captured placement, network and deployment type",
            region: "ap-southeast-4",
            source: multi_az_source,
            request: json!({
                "vpcSubnetIds": ["subnet-zzz"],
                "vpcSecurityGroupIds": ["sg-zzz", "sg-yyy"],
                "publiclyAccessible": false,
                "networkType": "IPV4",
                "deploymentType": "SINGLE_AZ",
                "kmsKeyId": "alias/restored-key",
                "tags": { "env": "restored" },
            }),
            expected: vec![
                // Overridden by the request.
                ("vpcSubnetIds", json!(["subnet-zzz"])),
                ("vpcSecurityGroupIds", json!(["sg-zzz", "sg-yyy"])),
                ("publiclyAccessible", json!(false)),
                ("networkType", json!("IPV4")),
                ("deploymentType", json!("SINGLE_AZ")),
                ("kmsKeyId", json!("alias/restored-key")),
                // Taken from the backup, which the request said nothing about.
                ("dbInstanceType", json!("db.influx.large")),
                ("allocatedStorage", json!(400)),
                ("dbStorageType", json!("InfluxIOIncludedT3")),
            ],
            expected_tags: json!({ "env": "restored" }),
            expects_secondary_zone: false,
        },
    ]
}

/// RULE: `RestoreFromDbBackup` BUILDS a resource out of the backup it names — the
/// configuration the backup captured and, for a DB instance, the InfluxDB data and admin
/// user its own plane held at that instant — with every member the request names overriding
/// the captured one; the resource it produces is one of its own, RESTORING until the clock
/// settles it to AVAILABLE, and the resource the backup was taken from is left alone.
///
/// `resourceType` reports the family of that resource under this service's own name for it
/// (`db-instance`, `db-cluster`): the segment every ARN of the family carries and the name
/// its IAM resources are declared under.
///
/// The members asserted here are `RestoreFromDbBackupOutput`'s and `GetDbInstanceOutput`'s
/// (`smithy_query(query="operation", operation="RestoreFromDbBackup,GetDbInstance")`), and
/// every status named is a member of the modelled `Status` / `RestoreStatus` enums.
#[tokio::test]
async fn a_restore_builds_the_resource_the_backup_captured() {
    for case in restore_cases() {
        let label = case.label;
        // One router per row, so what the row asserts is about its own state alone.
        let (router, engine) = router_and_engine();
        let spec = (case.source)("restore-source");
        let source = create_source(&router, case.region, label, &spec).await;
        source.write(label, POINT_BEFORE_BACKUP).await;
        let backup_id = take_backup(
            &router,
            case.region,
            label,
            "restore-source-backup",
            &source.id,
        )
        .await;
        // Written after the backup was taken, so it is not part of what the backup holds.
        source.write(label, POINT_AFTER_BACKUP).await;

        let answered = ok(
            label,
            call_in(
                &router,
                case.region,
                "RestoreFromDbBackup",
                restore_request("restored-resource", &backup_id, &case.request),
            )
            .await,
        );
        let restored_id = answered["restoredDbResourceId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {label:?}: expected a `restoredDbResourceId`, got {answered}")
            })
            .to_string();
        assert_ne!(
            restored_id, source.id,
            "case {label:?}: a restore to a new resource must mint an id of its own"
        );
        assert_eq!(
            answered["restoreStatus"].as_str(),
            Some("RESTORING"),
            "case {label:?}: the restore reports its own status, got {answered}"
        );
        assert_eq!(
            answered["resourceType"].as_str(),
            Some("db-instance"),
            "case {label:?}: a single-node deployment type restores a DB instance, got \
             {answered}"
        );
        assert_eq!(
            answered["engineType"].as_str(),
            Some("INFLUXDB_V2"),
            "case {label:?}: the restored resource runs the engine the backup captured, \
             got {answered}"
        );

        // The restored resource is a resource of this account and Region: it reads back
        // through `GetDbInstance`, carrying the captured configuration under the name the
        // restore asked for.
        let record = ok(
            label,
            call_in(
                &router,
                case.region,
                "GetDbInstance",
                json!({ "identifier": &restored_id }),
            )
            .await,
        );
        assert_eq!(
            record["name"].as_str(),
            Some("restored-resource"),
            "case {label:?}: the restored record carries the name asked for, got {record}"
        );
        for (member, expected) in &case.expected {
            assert_eq!(
                &record[*member], expected,
                "case {label:?}: expected {member} to be {expected}, got {}",
                record[*member]
            );
        }
        assert_eq!(
            record["deploymentType"], answered["deploymentType"],
            "case {label:?}: the restore and the record must report one deployment type, \
             got {answered} and {record}"
        );
        assert_eq!(
            record["secondaryAvailabilityZone"].is_string(),
            case.expects_secondary_zone,
            "case {label:?}: a standby's zone is reported only for the Multi-AZ deployment \
             type, got {record}"
        );
        assert!(
            record["arn"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!(":{}:", case.region)),
            "case {label:?}: the restored record is scoped to the Region it was restored \
             in, got {record}"
        );

        // The tags of the restored resource are the ones the restore asked for.
        let listed = ok(
            label,
            call_in(
                &router,
                case.region,
                "ListTagsForResource",
                json!({ "resourceArn": record["arn"] }),
            )
            .await,
        );
        assert_eq!(
            listed["tags"],
            case.expected_tags,
            "case {label:?}: expected the tags {}, got {listed}",
            case.expected_tags
        );

        // Restoring is work the service does, so the record is RESTORING until the clock
        // settles it — and that settle is due before this client's next probe.
        let (settled, probes) =
            poll_to_available(&router, case.region, &restored_id, label, "RESTORING").await;
        assert!(
            probes <= MAX_PROBES,
            "case {label:?}: a client needed {probes} probes to see the restored instance \
             settled, expected at most {MAX_PROBES}"
        );

        // The data the backup captured is the data the restored resource holds — the point
        // written before the backup, and not the one written after it.
        assert_eq!(
            stored_points(&engine, &restored_id),
            vec![point_before_backup()],
            "case {label:?}: the restored instance must hold exactly the data the backup \
             captured"
        );
        assert_eq!(
            stored_points(&engine, &source.id),
            vec![point_before_backup(), point_after_backup()],
            "case {label:?}: the source instance must keep every point written to it"
        );

        // And it serves that data at the address it reports, to the admin user the backup
        // captured — the restore request carried no password of its own.
        let (restored_host, restored_port) = address(label, &settled);
        assert_ne!(
            restored_port, source.port,
            "case {label:?}: the restored instance must publish a socket of its own"
        );
        poll_health(label, &restored_host, restored_port).await;
        let signed_in = exchange(
            label,
            &restored_host,
            restored_port,
            "POST",
            "/api/v2/signin",
            &[("authorization", basic(&source.username, &source.password))],
            "",
        )
        .await;
        assert_eq!(
            signed_in.status(),
            204,
            "case {label:?}: the restored instance must authenticate the admin user the \
             backup captured, got {} with {:?}",
            signed_in.status(),
            signed_in.body()
        );
        let refused = exchange(
            label,
            &restored_host,
            restored_port,
            "POST",
            "/api/v2/signin",
            &[(
                "authorization",
                basic(&source.username, "anotherpassword9"),
            )],
            "",
        )
        .await;
        assert_eq!(
            refused.status(),
            401,
            "case {label:?}: the restored instance holds one admin user, got {} with {:?}",
            refused.status(),
            refused.body()
        );

        // The resource the backup was taken from is untouched, and holds a secret of its
        // own rather than the restored resource's.
        let source_record = ok(
            label,
            call_in(
                &router,
                case.region,
                "GetDbInstance",
                json!({ "identifier": &source.id }),
            )
            .await,
        );
        assert_eq!(
            source_record["status"].as_str(),
            Some("AVAILABLE"),
            "case {label:?}: a restore must leave the source resource as it was, got \
             {source_record}"
        );
        assert_ne!(
            source_record["influxAuthParametersSecretArn"],
            record["influxAuthParametersSecretArn"],
            "case {label:?}: the restored resource holds its own authorization secret"
        );
    }
}

/// RULE (the same one, where the source is gone): a backup is a snapshot and not a
/// reference, so it restores a whole resource after the resource it was taken from has been
/// deleted.
#[tokio::test]
async fn a_backup_restores_a_resource_after_its_source_is_deleted() {
    let case = "a source deleted before the restore";
    let (router, engine) = router_and_engine();
    let source = create_source(&router, "us-west-2", case, &minimal_source("doomed-source")).await;
    source.write(case, POINT_BEFORE_BACKUP).await;
    let backup_id = take_backup(&router, "us-west-2", case, "doomed-backup", &source.id).await;

    ok(
        case,
        call_in(
            &router,
            "us-west-2",
            "DeleteDbInstance",
            json!({ "identifier": &source.id }),
        )
        .await,
    );
    let (status, gone) = call_in(
        &router,
        "us-west-2",
        "GetDbInstance",
        json!({ "identifier": &source.id }),
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: the source must be gone before the restore, got {status} with {gone}"
    );

    let answered = ok(
        case,
        call_in(
            &router,
            "us-west-2",
            "RestoreFromDbBackup",
            json!({ "name": "revived-source", "dbBackupId": &backup_id }),
        )
        .await,
    );
    let restored_id = answered["restoredDbResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a `restoredDbResourceId`, got {answered}"))
        .to_string();
    let (settled, _) = poll_to_available(&router, "us-west-2", &restored_id, case, "RESTORING").await;
    assert_eq!(
        settled["dbInstanceType"].as_str(),
        Some("db.influx.medium"),
        "case {case:?}: the restored resource carries the deleted source's configuration, \
         got {settled}"
    );
    assert_eq!(
        stored_points(&engine, &restored_id),
        vec![point_before_backup()],
        "case {case:?}: the restored resource holds the data the backup captured"
    );
    let (host, port) = address(case, &settled);
    poll_health(case, &host, port).await;
}

/// RULE (the same one, in the other restore mode): `REPLACE_EXISTING` restores INTO the
/// resource the name resolves to, so that resource keeps its id, its ARN and the address a
/// client is already dialling, and its data goes back to what the backup captured.
#[tokio::test]
async fn replacing_an_existing_resource_restores_into_it_and_keeps_its_identity() {
    let case = "a restore replacing the resource it was taken from";
    let (router, engine) = router_and_engine();
    let source = create_source(&router, "us-west-2", case, &minimal_source("in-place")).await;
    source.write(case, POINT_BEFORE_BACKUP).await;
    let backup_id = take_backup(&router, "us-west-2", case, "in-place-backup", &source.id).await;
    source.write(case, POINT_AFTER_BACKUP).await;
    let before = ok(
        case,
        call_in(
            &router,
            "us-west-2",
            "GetDbInstance",
            json!({ "identifier": &source.id }),
        )
        .await,
    );

    let answered = ok(
        case,
        call_in(
            &router,
            "us-west-2",
            "RestoreFromDbBackup",
            json!({
                "name": "in-place",
                "dbBackupId": &backup_id,
                "restoreMode": "REPLACE_EXISTING",
            }),
        )
        .await,
    );
    assert_eq!(
        answered["restoredDbResourceId"].as_str(),
        Some(source.id.as_str()),
        "case {case:?}: replacing an existing resource restores into THAT resource, got \
         {answered}"
    );
    assert_eq!(
        answered["restoreStatus"].as_str(),
        Some("RESTORING"),
        "case {case:?}: expected the restore to report its own status, got {answered}"
    );

    let (settled, _) =
        poll_to_available(&router, "us-west-2", &source.id, case, "RESTORING").await;
    assert_eq!(
        settled["arn"], before["arn"],
        "case {case:?}: the replaced resource keeps its ARN, got {settled}"
    );
    assert_eq!(
        (settled["endpoint"].clone(), settled["port"].clone()),
        (before["endpoint"].clone(), before["port"].clone()),
        "case {case:?}: the replaced resource keeps the address a client dials, got {settled}"
    );
    // The point written after the backup was taken is not part of it, so restoring into the
    // resource leaves exactly the data the backup holds.
    assert_eq!(
        stored_points(&engine, &source.id),
        vec![point_before_backup()],
        "case {case:?}: the replaced resource holds exactly the captured data"
    );
    // The address it kept still serves that data, to the same admin user.
    let (host, port) = address(case, &settled);
    poll_health(case, &host, port).await;
    let signed_in = exchange(
        case,
        &host,
        port,
        "POST",
        "/api/v2/signin",
        &[("authorization", basic(&source.username, &source.password))],
        "",
    )
    .await;
    assert_eq!(
        signed_in.status(),
        204,
        "case {case:?}: the kept address must authenticate the admin user, got {} with {:?}",
        signed_in.status(),
        signed_in.body()
    );
    // And exactly one instance still owns the name: a replace mints nothing.
    let listed = ok(
        case,
        call_in(&router, "us-west-2", "ListDbInstances", json!({})).await,
    );
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: a replace must leave one instance, got {listed}"
    );
}

/// RULE (the same one, where the deployment type names the other family): the restored
/// resource belongs to the family its deployment type belongs to, because the modelled
/// `ResourceDeploymentType` spans both — so a restore asking for the read-replicas
/// deployment type restores a DB CLUSTER, composed of DB instances of this store, and that
/// availability setting is gated behind the same AWS Marketplace subscription a create of
/// it is.
#[tokio::test]
async fn a_restore_asking_for_the_cluster_deployment_type_restores_a_db_cluster() {
    let case = "a backup restored as a read-replicas cluster";
    let (router, engine) = router_and_engine();
    let source = create_source(&router, "us-west-2", case, &minimal_source("cluster-source")).await;
    let backup_id = take_backup(&router, "us-west-2", case, "cluster-backup", &source.id).await;
    let request = json!({
        "name": "restored-cluster",
        "dbBackupId": &backup_id,
        "deploymentType": "MULTI_NODE_READ_REPLICAS",
    });

    // The account holds no subscription yet, so the gated availability setting is refused —
    // and nothing is restored.
    assert_missing_subscription_refusal(
        case,
        call_in(&router, "us-west-2", "RestoreFromDbBackup", request.clone()).await,
    );
    let listed = ok(
        case,
        call_in(&router, "us-west-2", "ListDbClusters", json!({})).await,
    );
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a refused restore must persist no cluster, got {listed}"
    );

    influxdb::state::subscribe_account_to_marketplace_offering(
        &*engine,
        influxdb::state::READ_REPLICAS_OFFERING,
    )
    .expect("the shared engine holds this service's store");

    let answered = ok(
        case,
        call_in(&router, "us-west-2", "RestoreFromDbBackup", request).await,
    );
    assert_eq!(
        answered["resourceType"].as_str(),
        Some("db-cluster"),
        "case {case:?}: the read-replicas deployment type is a cluster's, got {answered}"
    );
    assert_eq!(
        answered["deploymentType"].as_str(),
        Some("MULTI_NODE_READ_REPLICAS"),
        "case {case:?}: expected the deployment type asked for, got {answered}"
    );
    let cluster_id = answered["restoredDbResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a `restoredDbResourceId`, got {answered}"))
        .to_string();

    // It reads back as a DB cluster, carrying the configuration the backup captured.
    let cluster = ok(
        case,
        call_in(
            &router,
            "us-west-2",
            "GetDbCluster",
            json!({ "dbClusterId": &cluster_id }),
        )
        .await,
    );
    assert_eq!(
        cluster["name"].as_str(),
        Some("restored-cluster"),
        "case {case:?}: the restored cluster carries the name asked for, got {cluster}"
    );
    assert_eq!(
        cluster["dbInstanceType"].as_str(),
        Some("db.influx.medium"),
        "case {case:?}: the restored cluster runs on the captured instance type, got \
         {cluster}"
    );
    assert_eq!(
        cluster["engineType"].as_str(),
        Some("INFLUXDB_V2"),
        "case {case:?}: the restored cluster runs the captured engine, got {cluster}"
    );
    assert_eq!(
        cluster["allocatedStorage"].as_i64(),
        Some(20),
        "case {case:?}: the restored cluster takes the captured storage, got {cluster}"
    );

    // The nodes it is composed of are DB instances of this store, each reporting the
    // cluster it belongs to.
    let instances = ok(
        case,
        call_in(&router, "us-west-2", "ListDbInstances", json!({})).await,
    );
    let mut nodes: Vec<String> = Vec::new();
    for item in instances["items"].as_array().unwrap_or(&Vec::new()) {
        let id = id_of(case, item);
        let node = ok(
            case,
            call_in(
                &router,
                "us-west-2",
                "GetDbInstance",
                json!({ "identifier": &id }),
            )
            .await,
        );
        if node["dbClusterId"].as_str() == Some(cluster_id.as_str()) {
            assert!(
                node["instanceModes"]
                    .as_array()
                    .is_some_and(|modes| !modes.is_empty()),
                "case {case:?}: a cluster's node takes a role in it, got {node}"
            );
            nodes.push(id);
        }
    }
    assert!(
        nodes.len() >= 2,
        "case {case:?}: a read-replicas cluster is composed of a node that ingests and at \
         least one read replica, got {nodes:?}"
    );

    // And the cluster settles on the clock, after the nodes it provisioned.
    let started = Instant::now();
    let mut reported = cluster;
    while reported["status"].as_str() != Some("AVAILABLE") {
        assert_eq!(
            reported["status"].as_str(),
            Some("RESTORING"),
            "case {case:?}: a cluster coming up must report RESTORING until the clock \
             settles it, got {reported}"
        );
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the clock-owed settle never happened, last answer {reported}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
        reported = ok(
            case,
            call_in(
                &router,
                "us-west-2",
                "GetDbCluster",
                json!({ "dbClusterId": &cluster_id }),
            )
            .await,
        );
    }
    for id in &nodes {
        let node = ok(
            case,
            call_in(
                &router,
                "us-west-2",
                "GetDbInstance",
                json!({ "identifier": id }),
            )
            .await,
        );
        assert_eq!(
            node["status"].as_str(),
            Some("AVAILABLE"),
            "case {case:?}: a settled cluster's nodes are settled too, got {node}"
        );
    }
}

/// One refusal row: the restore request, and the refusal it must get.
struct RefusalCase {
    label: &'static str,
    request: Value,
    status: u16,
    error: &'static str,
    /// The whole message the refusal must carry, or `None` where the row pins only the
    /// error class.
    message: Option<String>,
}

/// RULE: a restore refuses what it cannot build — a backup identifier nothing resolves and
/// a name that does not answer the restore mode's question about it — with the modelled
/// error of that class, and persists nothing.
#[tokio::test]
async fn a_restore_refuses_a_backup_or_a_name_that_does_not_resolve() {
    let case = "the refusals";
    let (router, engine) = router_and_engine();
    influxdb::state::subscribe_account_to_marketplace_offering(
        &*engine,
        influxdb::state::READ_REPLICAS_OFFERING,
    )
    .expect("the shared engine holds this service's store");
    let source = create_source(&router, "us-west-2", case, &minimal_source("taken-name")).await;
    let backup_id = take_backup(&router, "us-west-2", case, "refusal-backup", &source.id).await;

    // An id of the modelled `DbBackupId` shape (3..64, `^[a-zA-Z0-9]+$`) that this store
    // never minted.
    let unknown_backup = "deadbeef00";
    let rows = vec![
        RefusalCase {
            label: "a backup identifier nothing resolves",
            request: json!({ "name": "nowhere", "dbBackupId": unknown_backup }),
            status: 404,
            error: "ResourceNotFoundException",
            message: Some(format!("DB backup {unknown_backup} not found")),
        },
        RefusalCase {
            label: "a new resource under a name a live instance holds",
            request: json!({ "name": "taken-name", "dbBackupId": &backup_id }),
            status: 409,
            error: "ConflictException",
            message: Some(format!(
                "The DbInstance with id [{}], name [taken-name], customer [{ACCOUNT}] \
                 already exists",
                source.id
            )),
        },
        RefusalCase {
            label: "replacing a resource no name resolves to",
            request: json!({
                "name": "never-created",
                "dbBackupId": &backup_id,
                "restoreMode": "REPLACE_EXISTING",
            }),
            status: 404,
            error: "ResourceNotFoundException",
            message: Some(format!(
                "The DbInstance with name [never-created], customer [{ACCOUNT}] does not \
                 exist"
            )),
        },
        RefusalCase {
            label: "replacing a cluster no name resolves to",
            request: json!({
                "name": "never-created",
                "dbBackupId": &backup_id,
                "restoreMode": "REPLACE_EXISTING",
                "deploymentType": "MULTI_NODE_READ_REPLICAS",
            }),
            status: 404,
            error: "ResourceNotFoundException",
            message: Some(format!(
                "The DbCluster with name [never-created], customer [{ACCOUNT}] does not \
                 exist"
            )),
        },
        RefusalCase {
            label: "a port InfluxDB does not accept connections on",
            request: json!({
                "name": "unavailable-port",
                "dbBackupId": &backup_id,
                "port": 8090,
            }),
            status: 400,
            error: "ValidationException",
            message: Some(
                "The port [8090] is not available. The value can't be 2375-2376, \
                 7788-7799, 8090, or 51678-51680"
                    .to_string(),
            ),
        },
    ];

    for row in rows {
        let label = row.label;
        let (status, body) = call_in(&router, "us-west-2", "RestoreFromDbBackup", row.request).await;
        assert_eq!(
            status, row.status,
            "case {label:?}: expected HTTP {}, got {status} with {body}",
            row.status
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with(row.error),
            "case {label:?}: expected a {}, got {body}",
            row.error
        );
        if let Some(expected) = &row.message {
            assert_eq!(
                body["message"].as_str(),
                Some(expected.as_str()),
                "case {label:?}: unexpected message {:?}",
                body["message"]
            );
        }
    }

    // Every refusal above left the store exactly as it was: the source instance, and
    // nothing else.
    let listed = ok(
        case,
        call_in(&router, "us-west-2", "ListDbInstances", json!({})).await,
    );
    assert_eq!(
        listed["items"]
            .as_array()
            .map(|items| items.iter().map(|item| id_of(case, item)).collect()),
        Some(vec![source.id.clone()]),
        "case {case:?}: a refused restore must persist no resource, got {listed}"
    );
    let clusters = ok(
        case,
        call_in(&router, "us-west-2", "ListDbClusters", json!({})).await,
    );
    assert_eq!(
        clusters["items"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a refused restore must persist no cluster, got {clusters}"
    );
}
