// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the two rules a replication configuration's
//! `versionToken` and its ABSENCE are governed by, on every route of this
//! service that holds a replication configuration — a single table's and a table
//! bucket's alike.
//!
//! RULE 1 (the token's class): a replication configuration's `versionToken` is an
//! opaque server-minted handle in the one token form this service mints a version
//! token in — a single unbroken run of lowercase hexadecimal characters of a
//! fixed width ([`assert_table_version_token`]), carrying no separator and no
//! structure a client could read anything out of, so it is NOT an identifier's
//! hyphenated-UUID spelling. The SAME value is then echoed by the read of the
//! configuration and accepted as the precondition token by the next write of it,
//! and every write mints a fresh one, so a caller holding the token it was handed
//! can tell a later write apart from its own. The model constrains the member
//! only by type (`smithy_query(query="operation",
//! operation="PutTableReplication")` reports `versionToken` as a `String` on both
//! the input and the required output), so the form is this service's behaviour
//! and is stated in one place for every operation that mints one.
//!
//! RULE 2 (absence is observable): a table — or a table bucket — has a
//! replication configuration only while one is stored for it, so the read reports
//! one only then; otherwise it is refused with the modelled `NotFoundException`
//! (`@httpError` 404, serialized with the `x-amzn-errortype` header), stated with
//! the one fixed sentence this service states that class with, which interpolates
//! nothing from the request or from state. That holds whichever way the resource
//! came to have none — never configured, or configured and then removed, however
//! many writes preceded the removal — and a refused read leaves the store as it
//! was, so the refusal repeats.
//!
//! The model bounds the shapes the rows span
//! (`smithy_query(query="shape", shape="TableReplicationConfiguration")`):
//! `rules` is `@length(1, 1)`, a rule's `destinations` `@length(1, 5)` and `role`
//! an `IAMRole` (`^arn:.+:iam::[0-9]{12}:role/.+$`). So the rows differ in the
//! scope the configuration is held at, the region, the role, how many
//! destinations the one rule carries — one, several and the modelled maximum of
//! five — and how many successive writes precede the read and the removal. None
//! of those may change the class of the token or the refusal an absent
//! configuration is answered with.

use serde_json::{json, Value};

use super::harness::{Emulator, Reply};
use super::table_record_expiration::create_table;
use super::wire_forms::assert_table_version_token;

/// The fixed sentence this service states a resource that has no replication
/// configuration with, which is entirely fixed text for the class.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// Which record a row's configuration is held on: one table's own, or the table
/// bucket's. The two are independent records of the same kind, written and read
/// through operations of their own — and both mint their token through the one
/// generator RULE 1 is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Table,
    Bucket,
}

impl Scope {
    /// The query member each scope's operations address their resource by.
    fn member(self) -> &'static str {
        match self {
            Scope::Table => "tableArn",
            Scope::Bucket => "tableBucketARN",
        }
    }
}

/// One row: a replication configuration held at `scope` on a resource in
/// `region`, naming `role` and `destinations` destination table buckets, written
/// `writes` times.
#[derive(Debug)]
struct Case {
    label: &'static str,
    scope: Scope,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, numbers and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The table's name (`^[0-9a-z_]*$`), used by the table-scoped rows.
    table: &'static str,
    /// An IAM role ARN of the form the modelled `IAMRole` pattern accepts.
    role: &'static str,
    /// How many destination table buckets the one rule carries; the modelled
    /// `ReplicationDestinations` list is `@length(1, 5)`.
    destinations: usize,
    /// How many successive accepted writes the row performs, each presenting the
    /// token the previous one handed back.
    writes: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a table's own configuration, one destination, a single write",
            scope: Scope::Table,
            region: "us-east-1",
            bucket: "repl-token-east",
            table: "orders_east",
            role: "arn:aws:iam::000000000000:role/s3tables-replication-token",
            destinations: 1,
            writes: 1,
        },
        Case {
            label: "a table's own configuration, three destinations, two writes",
            scope: Scope::Table,
            region: "us-west-2",
            bucket: "repl-token-west",
            table: "clicks_west",
            role: "arn:aws:iam::000000000000:role/replication/west/nested-path",
            destinations: 3,
            writes: 2,
        },
        Case {
            label: "a table's own configuration, the modelled maximum of five destinations, three \
                    writes",
            scope: Scope::Table,
            region: "eu-west-1",
            bucket: "repl-token-eu",
            table: "sensor_readings",
            role: "arn:aws:iam::000000000000:role/replication-eu",
            destinations: 5,
            writes: 3,
        },
        Case {
            label: "a table bucket's configuration, one destination, two writes",
            scope: Scope::Bucket,
            region: "ap-south-1",
            bucket: "repl-token-ap",
            table: "unused_ap",
            role: "arn:aws:iam::000000000000:role/replication-ap",
            destinations: 1,
            writes: 2,
        },
        Case {
            label: "a table bucket's configuration, five destinations, a single write",
            scope: Scope::Bucket,
            region: "us-east-2",
            bucket: "repl-token-central",
            table: "unused_central",
            role: "arn:aws:iam::000000000000:role/replication-central",
            destinations: 5,
            writes: 1,
        },
    ]
}

/// A configuration whose ONE rule — the modelled `rules` list is
/// `@length(1, 1)` — replicates to `destinations`.
fn configuration(role: &str, destinations: &[String]) -> Value {
    json!({
        "role": role,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<_>>(),
        }],
    })
}

/// Create `count` destination table buckets named after `prefix` in `region`, and
/// answer their ARNs.
async fn destination_buckets(
    emulator: &Emulator,
    region: &str,
    prefix: &str,
    count: usize,
    case: &str,
) -> Vec<String> {
    let mut arns = Vec::with_capacity(count);
    for index in 0..count {
        let name = format!("{prefix}-{index}");
        let created = emulator
            .create_table_bucket(region, &json!({ "name": name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the destination bucket {name:?} must succeed, got {status} \
             {body}",
            status = created.status,
            body = created.body
        );
        arns.push(created.text("arn"));
    }
    arns
}

/// The resource a row's configuration is held on: its ARN, at the row's scope.
async fn source_resource(emulator: &Emulator, case: &Case) -> String {
    match case.scope {
        // A table-scoped row needs the table itself, whose ARN carries the
        // bucket's.
        Scope::Table => create_table(emulator, case.region, case.bucket, case.table).await,
        Scope::Bucket => {
            let created = emulator
                .create_table_bucket(case.region, &json!({ "name": case.bucket }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: creating the source bucket must succeed, got {status} {body}",
                label = case.label,
                status = created.status,
                body = created.body
            );
            created.text("arn")
        }
    }
}

/// Write the configuration at `scope`, presenting `token` as the precondition
/// when the row holds one.
async fn put(
    emulator: &Emulator,
    scope: Scope,
    region: &str,
    arn: &str,
    token: Option<&str>,
    settings: &Value,
) -> Reply {
    let mut query = vec![(scope.member(), arn.to_string())];
    if let Some(presented) = token {
        query.push(("versionToken", presented.to_string()));
    }
    match scope {
        Scope::Table => emulator.put_table_replication(region, &query, settings).await,
        Scope::Bucket => {
            emulator
                .put_table_bucket_replication(region, &query, settings)
                .await
        }
    }
}

/// Read the configuration at `scope`.
async fn read(emulator: &Emulator, scope: Scope, region: &str, arn: &str) -> Reply {
    match scope {
        Scope::Table => emulator.get_table_replication(region, arn).await,
        Scope::Bucket => emulator.get_table_bucket_replication(region, arn).await,
    }
}

/// Remove the configuration at `scope`, presenting `token` — which
/// `DeleteTableReplication` models as `@required` — as the precondition.
async fn remove(
    emulator: &Emulator,
    scope: Scope,
    region: &str,
    arn: &str,
    token: &str,
) -> Reply {
    let query = vec![
        (scope.member(), arn.to_string()),
        ("versionToken", token.to_string()),
    ];
    match scope {
        Scope::Table => emulator.delete_table_replication(region, &query).await,
        Scope::Bucket => emulator.delete_table_bucket_replication(region, &query).await,
    }
}

/// Assert `reply` is the modelled not-found of a resource that holds no
/// replication configuration: HTTP 404, the `NotFoundException` error type (which
/// the harness reads off the `x-amzn-errortype` header), the one fixed sentence,
/// and none of the success members.
fn assert_no_configuration(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected status 404, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected the NotFoundException error type, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        NO_REPLICATION_CONFIGURATION,
        "case {case:?}: expected the fixed sentence {NO_REPLICATION_CONFIGURATION:?}, got \
         {actual:?}",
        actual = reply.message()
    );
    assert!(
        reply.body.get("configuration").is_none() && reply.body.get("versionToken").is_none(),
        "case {case:?}: a refusal reports no configuration and no token, got {body}",
        body = reply.body
    );
}

#[tokio::test]
async fn every_replication_version_token_is_an_opaque_handle_in_the_services_token_form() {
    let emulator = Emulator::new();
    for case in cases() {
        let label = case.label;
        let arn = source_resource(&emulator, &case).await;
        let destinations = destination_buckets(
            &emulator,
            case.region,
            &format!("{bucket}-dst", bucket = case.bucket),
            case.destinations,
            label,
        )
        .await;
        let settings = configuration(case.role, &destinations);

        // Each write presents the token the previous one handed back, so the
        // token the service minted is observed to be ACCEPTED as the
        // precondition handle and not merely reported.
        let mut minted: Vec<String> = Vec::new();
        for write in 0..case.writes {
            let applied = put(
                &emulator,
                case.scope,
                case.region,
                &arn,
                minted.last().map(String::as_str),
                &settings,
            )
            .await;
            assert_eq!(
                applied.status, 200,
                "case {label:?}: write {write} must be accepted, got {status} {body}",
                status = applied.status,
                body = applied.body
            );
            let token = applied.text("versionToken");
            assert_table_version_token(
                &token,
                "the versionToken an accepted replication put mints",
                label,
            );
            assert!(
                !minted.contains(&token),
                "case {label:?}: write {write} re-used the token {token:?}, which an earlier \
                 write already minted"
            );

            // The read echoes the SAME handle, in the same form: the token a
            // client stores is the one the service reports.
            let reported = read(&emulator, case.scope, case.region, &arn).await;
            assert_eq!(
                reported.status, 200,
                "case {label:?}: the read after write {write} must be served, got {status} {body}",
                status = reported.status,
                body = reported.body
            );
            let echoed = reported.text("versionToken");
            assert_table_version_token(
                &echoed,
                "the versionToken the read of a replication configuration echoes",
                label,
            );
            assert_eq!(
                echoed, token,
                "case {label:?}: the read must echo the token write {write} minted, got \
                 {echoed:?}"
            );
            minted.push(token);
        }

        // And the handle the last write minted is what removes the
        // configuration, so the whole precondition round trip runs on a value of
        // this form.
        let current = minted
            .last()
            .expect("every row performs at least one write")
            .clone();
        let removed = remove(&emulator, case.scope, case.region, &arn, &current).await;
        assert_eq!(
            removed.status, 204,
            "case {label:?}: the current token must remove the configuration, got {status} {body}",
            status = removed.status,
            body = removed.body
        );
        assert_no_configuration(
            &read(&emulator, case.scope, case.region, &arn).await,
            &format!("{label}, the read after the removal"),
        );
    }
}

/// How a row's resource comes to hold no replication configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Absence {
    /// Nothing was ever written for it.
    NeverConfigured,
    /// `writes` configurations were written and the last one removed.
    Removed { writes: usize },
    /// A configuration was written, removed, written afresh and removed again,
    /// so the absence is reached a second time.
    RemovedTwice,
}

/// One row of RULE 2: the scope the read addresses and how the resource came to
/// hold no configuration.
#[derive(Debug)]
struct AbsenceCase {
    label: &'static str,
    scope: Scope,
    region: &'static str,
    bucket: &'static str,
    table: &'static str,
    role: &'static str,
    destinations: usize,
    absence: Absence,
}

fn absence_cases() -> Vec<AbsenceCase> {
    vec![
        AbsenceCase {
            label: "a table that was never configured",
            scope: Scope::Table,
            region: "us-east-1",
            bucket: "repl-absent-east",
            table: "never_east",
            role: "arn:aws:iam::000000000000:role/replication-absent-east",
            destinations: 1,
            absence: Absence::NeverConfigured,
        },
        AbsenceCase {
            label: "a table whose single configuration was removed",
            scope: Scope::Table,
            region: "us-west-2",
            bucket: "repl-absent-west",
            table: "removed_west",
            role: "arn:aws:iam::000000000000:role/replication-absent-west",
            destinations: 2,
            absence: Absence::Removed { writes: 1 },
        },
        AbsenceCase {
            label: "a table configured three times, then removed",
            scope: Scope::Table,
            region: "eu-west-1",
            bucket: "repl-absent-eu",
            table: "removed_eu",
            role: "arn:aws:iam::000000000000:role/replication-absent-eu",
            destinations: 5,
            absence: Absence::Removed { writes: 3 },
        },
        AbsenceCase {
            label: "a table configured and removed twice over",
            scope: Scope::Table,
            region: "ap-south-1",
            bucket: "repl-absent-ap",
            table: "recreated_ap",
            role: "arn:aws:iam::000000000000:role/replication-absent-ap",
            destinations: 1,
            absence: Absence::RemovedTwice,
        },
        AbsenceCase {
            label: "a table bucket that was never configured",
            scope: Scope::Bucket,
            region: "us-east-2",
            bucket: "repl-absent-bucket-new",
            table: "unused_bucket_new",
            role: "arn:aws:iam::000000000000:role/replication-absent-bucket",
            destinations: 1,
            absence: Absence::NeverConfigured,
        },
        AbsenceCase {
            label: "a table bucket whose configuration was removed",
            scope: Scope::Bucket,
            region: "eu-central-1",
            bucket: "repl-absent-bucket-gone",
            table: "unused_bucket_gone",
            role: "arn:aws:iam::000000000000:role/replication-absent-bucket-gone",
            destinations: 3,
            absence: Absence::Removed { writes: 2 },
        },
    ]
}

#[tokio::test]
async fn a_resource_with_no_replication_configuration_refuses_the_read_with_the_fixed_not_found() {
    let emulator = Emulator::new();
    for row in absence_cases() {
        let label = row.label;
        let case = Case {
            label,
            scope: row.scope,
            region: row.region,
            bucket: row.bucket,
            table: row.table,
            role: row.role,
            destinations: row.destinations,
            writes: 1,
        };
        let arn = source_resource(&emulator, &case).await;

        // How many write-then-remove cycles the row runs, and how many writes
        // each one applies before the removal.
        let (cycles, writes) = match row.absence {
            Absence::NeverConfigured => (0, 0),
            Absence::Removed { writes } => (1, writes),
            Absence::RemovedTwice => (2, 1),
        };
        if cycles > 0 {
            let destinations = destination_buckets(
                &emulator,
                row.region,
                &format!("{bucket}-dst", bucket = row.bucket),
                row.destinations,
                label,
            )
            .await;
            let settings = configuration(row.role, &destinations);
            for cycle in 0..cycles {
                let mut current: Option<String> = None;
                for write in 0..writes {
                    let applied = put(
                        &emulator,
                        row.scope,
                        row.region,
                        &arn,
                        current.as_deref(),
                        &settings,
                    )
                    .await;
                    assert_eq!(
                        applied.status, 200,
                        "case {label:?}: cycle {cycle} write {write} must be accepted, got \
                         {status} {body}",
                        status = applied.status,
                        body = applied.body
                    );
                    current = Some(applied.text("versionToken"));
                }
                // While a configuration IS stored, the read reports it: the
                // refusal below is about absence and not about the route.
                let configured = read(&emulator, row.scope, row.region, &arn).await;
                assert_eq!(
                    configured.status, 200,
                    "case {label:?}: cycle {cycle}: a stored configuration must be reported, got \
                     {status} {body}",
                    status = configured.status,
                    body = configured.body
                );
                let token = current.expect("a cycle applies at least one write");
                let removed = remove(&emulator, row.scope, row.region, &arn, &token).await;
                assert_eq!(
                    removed.status, 204,
                    "case {label:?}: cycle {cycle}: the removal must be accepted, got {status} \
                     {body}",
                    status = removed.status,
                    body = removed.body
                );
            }
        }

        // Reading twice over: a refused read changes nothing a later caller
        // sees, so the same refusal is stated again.
        for attempt in 1..=2 {
            let refused = read(&emulator, row.scope, row.region, &arn).await;
            assert_no_configuration(&refused, &format!("{label}, read {attempt}"));
        }
    }
}
