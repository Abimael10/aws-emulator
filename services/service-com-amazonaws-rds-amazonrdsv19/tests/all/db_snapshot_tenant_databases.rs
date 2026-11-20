// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeDBSnapshotTenantDatabases`.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a DB snapshot CAPTURES the tenant databases its source DB instance
//! held at the instant the snapshot was taken, and this report computes its list from those
//! captured records — narrowed by the snapshot, the source DB instance, the instance's
//! resource id, the snapshot type and each documented filter — so a tenant database created
//! or deleted after the snapshot was taken does not change what the snapshot reports. The
//! command "only applies to RDS for Oracle DB instances in the multi-tenant configuration",
//! so those are the snapshots a named `DBSnapshotIdentifier` is resolved among: naming the
//! snapshot of a single-tenant instance is the modelled `DBSnapshotNotFoundFault`, while a
//! request that names no snapshot reports on the multi-tenant ones and leaves the others out.
//!
//! The tables below span what the rule generalises over: the container-database engines a
//! multi-tenant instance runs and a single-tenant engine, instances holding none, one and
//! several tenant databases, tenant databases created out of name order and with and
//! without the two character-set members, each selector and each documented filter name
//! (by identifier and by ARN where the member accepts both), each published `MaxRecords`
//! bound and one value past it, and a report longer than one page.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The instance class every instance below is created with; which class an instance runs on
/// is nothing this report reads.
const INSTANCE_CLASS: &str = "db.m5.large";

/// The master username every tenant database below is created with, unless its row names
/// another.
const TENANT_MASTER_USERNAME: &str = "tenantadmin";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, OWNED, so a table row can hold them and a caller can
/// add to them.
macro_rules! owned_params {
    ($($name:expr => $value:expr),* $(,)?) => {
        params![$($name => $value),*].to_vec()
    };
}

/// One tenant database a row creates before its snapshot is taken.
struct Tenant {
    name: &'static str,
    master_username: &'static str,
    /// `CharacterSetName`, absent for a tenant database created without one.
    character_set: Option<&'static str>,
    /// `NcharCharacterSetName`, absent for the same reason.
    nchar_character_set: Option<&'static str>,
}

/// One row of the capture table: a DB instance, the engine it runs, the snapshot taken of
/// it and the tenant databases it holds when that snapshot is taken.
struct CaptureCase {
    case: &'static str,
    instance: &'static str,
    engine: &'static str,
    snapshot: &'static str,
    tenants: &'static [Tenant],
}

/// `CreateDBInstance` followed by a poll loop until the instance reports `available`, which
/// is the state a snapshot and a tenant database can be created in. Answers the instance's
/// `DbiResourceId`, the id this report selects on.
async fn create_available_instance(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
) -> String {
    expect_ok(
        &format!("create instance {identifier}"),
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => engine,
                "DBInstanceClass" => INSTANCE_CLASS,
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => identifier];
    let (body, _) = poll_until(
        identifier,
        "the DB instance reaching available",
        || call(router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    text(&body, "DbiResourceId")
}

/// `CreateTenantDatabase` for one tenant database, answering the
/// `TenantDatabaseResourceId` the create minted.
async fn create_tenant(
    router: &MultiServiceRouter,
    instance: &str,
    tenant: &Tenant,
) -> String {
    let mut parameters = owned_params![
        "DBInstanceIdentifier" => instance,
        "TenantDBName" => tenant.name,
        "MasterUsername" => tenant.master_username,
        "MasterUserPassword" => "a-secret-password",
    ];
    if let Some(character_set) = tenant.character_set {
        parameters.push(("CharacterSetName", character_set.to_string()));
    }
    if let Some(nchar) = tenant.nchar_character_set {
        parameters.push(("NcharCharacterSetName", nchar.to_string()));
    }
    let created = expect_ok(
        &format!("create tenant database {} in {instance}", tenant.name),
        call(router, "CreateTenantDatabase", &parameters).await,
    );
    text(&created, "TenantDatabaseResourceId")
}

/// `CreateDBSnapshot` followed by a poll loop until the snapshot reports `available`.
async fn take_available_snapshot(router: &MultiServiceRouter, instance: &str, snapshot: &str) {
    expect_ok(
        &format!("take snapshot {snapshot} of {instance}"),
        call(
            router,
            "CreateDBSnapshot",
            &params![
                "DBSnapshotIdentifier" => snapshot,
                "DBInstanceIdentifier" => instance,
            ],
        )
        .await,
    );
    let read = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        snapshot,
        "the DB snapshot reaching available",
        || call(router, "DescribeDBSnapshots", &read),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some("available"),
    )
    .await;
}

/// `DescribeDBSnapshotTenantDatabases`, asserting the 200 and answering the body.
async fn describe(
    router: &MultiServiceRouter,
    case: &str,
    parameters: &[(&str, String)],
) -> String {
    expect_ok(
        case,
        call(router, "DescribeDBSnapshotTenantDatabases", parameters).await,
    )
}

/// The `TenantDBName` of every tenant database a report carries, in the order it reported
/// them.
fn reported_names(body: &str) -> Vec<String> {
    texts(body, "TenantDBName")
}

/// The one `<DBSnapshotTenantDatabase>` subtree a report carries for `tenant_db_name`.
fn the_reported(case: &str, body: &str, tenant_db_name: &str) -> String {
    let found: Vec<String> = subtrees(body, "DBSnapshotTenantDatabase")
        .into_iter()
        .filter(|subtree| text(subtree, "TenantDBName") == tenant_db_name)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one reported {tenant_db_name}, got {} in {body}",
        found.len()
    );
    found.into_iter().next().expect("checked just above")
}

#[tokio::test]
async fn a_snapshot_reports_the_tenant_databases_it_captured() {
    // The rows differ along what the rule generalises over: the container-database engines
    // a multi-tenant instance runs (`CreateDBInstance$MultiTenant` "only applies to RDS for
    // Oracle container database (CDB) engines", whose engine names end in `-cdb`), a
    // single-tenant engine, how many tenant databases the instance holds, the order they
    // were created in, and whether they carry the two character-set members.
    let table: &[CaptureCase] = &[
        CaptureCase {
            case: "one tenant database on an Oracle Enterprise Edition CDB instance",
            instance: "emu-stdb-ee",
            engine: "oracle-ee-cdb",
            snapshot: "emu-stdb-ee-snap",
            tenants: &[Tenant {
                name: "tenant1",
                master_username: TENANT_MASTER_USERNAME,
                character_set: Some("AL32UTF8"),
                nchar_character_set: Some("AL16UTF16"),
            }],
        },
        CaptureCase {
            case: "three tenant databases created out of name order, each with its own \
                   master user",
            instance: "emu-stdb-se2",
            engine: "oracle-se2-cdb",
            snapshot: "emu-stdb-se2-snap",
            tenants: &[
                Tenant {
                    name: "gamma",
                    master_username: "gammaadmin",
                    character_set: Some("WE8ISO8859P15"),
                    nchar_character_set: None,
                },
                Tenant {
                    name: "alpha",
                    master_username: "alphaadmin",
                    character_set: None,
                    nchar_character_set: None,
                },
                Tenant {
                    name: "beta",
                    master_username: "betaadmin",
                    character_set: Some("AL32UTF8"),
                    nchar_character_set: Some("UTF8"),
                },
            ],
        },
        CaptureCase {
            case: "a multi-tenant instance that holds no tenant database yet",
            instance: "emu-stdb-empty",
            engine: "oracle-ee-cdb",
            snapshot: "emu-stdb-empty-snap",
            tenants: &[],
        },
        CaptureCase {
            case: "a single-tenant engine, which holds no tenant databases at all",
            instance: "emu-stdb-postgres",
            engine: "postgres",
            snapshot: "emu-stdb-postgres-snap",
            tenants: &[],
        },
    ];

    let router = emulator();
    for row in table {
        let case = row.case;
        let dbi_resource_id = create_available_instance(&router, row.instance, row.engine).await;
        let mut minted: Vec<(&str, String)> = Vec::new();
        for tenant in row.tenants {
            minted.push((tenant.name, create_tenant(&router, row.instance, tenant).await));
        }
        take_available_snapshot(&router, row.instance, row.snapshot).await;

        // "This command only applies to RDS for Oracle DB instances in the multi-tenant
        // configuration", whose engine names end in `-cdb`. Naming the snapshot of a
        // single-tenant instance therefore resolves no snapshot of this command's own, and
        // is refused exactly as a never-created identifier is — so a caller can tell "this
        // snapshot holds no tenant database" from "this is not a multi-tenant snapshot".
        if !row.engine.ends_with("-cdb") {
            let refused = expect_error(
                case,
                call(
                    &router,
                    "DescribeDBSnapshotTenantDatabases",
                    &params!["DBSnapshotIdentifier" => row.snapshot],
                )
                .await,
                404,
                "DBSnapshotNotFound",
            );
            assert_eq!(
                text(&refused, "Message"),
                format!("DBSnapshot {} not found.", row.snapshot),
                "case {case:?}: the fault names the snapshot the request asked for: {refused}"
            );
            continue;
        }
        let body = describe(
            &router,
            case,
            &params!["DBSnapshotIdentifier" => row.snapshot],
        )
        .await;
        // The report is in the order the snapshot holds its captured tenant databases: by
        // tenant name, whatever order the creates arrived in.
        let mut expected: Vec<String> = row.tenants.iter().map(|t| t.name.to_string()).collect();
        expected.sort();
        assert_eq!(
            reported_names(&body),
            expected,
            "case {case:?}: the snapshot reports the tenant databases its instance held, \
             in name order: {body}"
        );

        for tenant in row.tenants {
            let reported = the_reported(case, &body, tenant.name);
            let resource_id = minted
                .iter()
                .find(|(name, _)| *name == tenant.name)
                .map(|(_, id)| id.clone())
                .expect("every created tenant database minted a resource id");
            for (member, expected, actual) in [
                ("DBSnapshotIdentifier", row.snapshot.to_string(), text(&reported, "DBSnapshotIdentifier")),
                ("DBInstanceIdentifier", row.instance.to_string(), text(&reported, "DBInstanceIdentifier")),
                ("DbiResourceId", dbi_resource_id.clone(), text(&reported, "DbiResourceId")),
                ("EngineName", row.engine.to_string(), text(&reported, "EngineName")),
                // Every snapshot a caller asks for is a manual one.
                ("SnapshotType", "manual".to_string(), text(&reported, "SnapshotType")),
                (
                    "MasterUsername",
                    tenant.master_username.to_string(),
                    text(&reported, "MasterUsername"),
                ),
                (
                    "TenantDatabaseResourceId",
                    resource_id,
                    text(&reported, "TenantDatabaseResourceId"),
                ),
                (
                    "DBSnapshotTenantDatabaseARN",
                    format!(
                        "arn:aws:rds:{REGION}:{ACCOUNT}:snapshot-tenant-database:{}:{}",
                        row.snapshot, tenant.name
                    ),
                    text(&reported, "DBSnapshotTenantDatabaseARN"),
                ),
            ] {
                assert_eq!(
                    actual, expected,
                    "case {case:?}: tenant {}: expected {member} {expected:?}, got \
                     {actual:?} in {reported}",
                    tenant.name
                );
            }
            assert_eq!(
                optional_text(&reported, "CharacterSetName").as_deref(),
                tenant.character_set,
                "case {case:?}: tenant {}: the captured CharacterSetName: {reported}",
                tenant.name
            );
            assert_eq!(
                optional_text(&reported, "NcharCharacterSetName").as_deref(),
                tenant.nchar_character_set,
                "case {case:?}: tenant {}: the captured NcharCharacterSetName: {reported}",
                tenant.name
            );
            assert!(
                optional_text(&reported, "TenantDatabaseCreateTime").is_some(),
                "case {case:?}: tenant {}: the report carries a creation time: {reported}",
                tenant.name
            );
        }

        // A report changes nothing a later caller sees: the same request answers the same
        // tenant databases.
        let again = describe(
            &router,
            case,
            &params!["DBSnapshotIdentifier" => row.snapshot],
        )
        .await;
        assert_eq!(
            subtrees(&again, "DBSnapshotTenantDatabase"),
            subtrees(&body, "DBSnapshotTenantDatabase"),
            "case {case:?}: reading the report twice must report the same records: {again}"
        );
    }
}

/// One row of the selector table: the parameters of one call and the tenant databases it
/// must report.
struct SelectorCase {
    case: &'static str,
    parameters: Vec<(&'static str, String)>,
    expected: Vec<&'static str>,
}

#[tokio::test]
async fn each_selector_and_filter_narrows_the_report() {
    let router = emulator();
    // Two multi-tenant instances, each with its own snapshot, so every selector below has
    // records on both sides of it.
    let first_dbi = create_available_instance(&router, "emu-sel-a", "oracle-ee-cdb").await;
    let second_dbi = create_available_instance(&router, "emu-sel-b", "oracle-se2-cdb").await;
    let mut resource_ids: Vec<(&str, String)> = Vec::new();
    for (instance, name) in [
        ("emu-sel-a", "alpha"),
        ("emu-sel-a", "bravo"),
        ("emu-sel-b", "charlie"),
    ] {
        let tenant = Tenant {
            name,
            master_username: TENANT_MASTER_USERNAME,
            character_set: None,
            nchar_character_set: None,
        };
        resource_ids.push((name, create_tenant(&router, instance, &tenant).await));
    }
    take_available_snapshot(&router, "emu-sel-a", "emu-sel-snap-a").await;
    take_available_snapshot(&router, "emu-sel-b", "emu-sel-snap-b").await;
    let alpha_resource_id = resource_ids[0].1.clone();

    // The rows differ along what the rule generalises over: each selector the input
    // documents and each filter name its `Filters` member documents, including the two that
    // accept an ARN beside an identifier, a filter carrying two values, and a value nothing
    // matches.
    let table: Vec<SelectorCase> = vec![
        SelectorCase {
            case: "no selector reports every snapshot's tenant databases",
            parameters: Vec::new(),
            expected: vec!["alpha", "bravo", "charlie"],
        },
        SelectorCase {
            case: "by snapshot",
            parameters: owned_params!["DBSnapshotIdentifier" => "emu-sel-snap-a"],
            expected: vec!["alpha", "bravo"],
        },
        SelectorCase {
            case: "by source DB instance",
            parameters: owned_params!["DBInstanceIdentifier" => "emu-sel-b"],
            expected: vec!["charlie"],
        },
        SelectorCase {
            // "This parameter isn't case-sensitive."
            case: "by source DB instance in another case",
            parameters: owned_params!["DBInstanceIdentifier" => "EMU-SEL-B"],
            expected: vec!["charlie"],
        },
        SelectorCase {
            case: "by the source instance's resource id",
            parameters: owned_params!["DbiResourceId" => first_dbi.clone()],
            expected: vec!["alpha", "bravo"],
        },
        SelectorCase {
            case: "by the snapshot type every caller-requested snapshot carries",
            parameters: owned_params!["SnapshotType" => "manual"],
            expected: vec!["alpha", "bravo", "charlie"],
        },
        SelectorCase {
            case: "by a snapshot type no snapshot was taken under",
            parameters: owned_params!["SnapshotType" => "automated"],
            expected: Vec::new(),
        },
        SelectorCase {
            case: "the tenant-db-name filter",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "bravo",
            ],
            expected: vec!["bravo"],
        },
        SelectorCase {
            case: "the tenant-db-name filter carrying two values",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "alpha",
                "Filters.Filter.1.Values.Value.2" => "charlie",
            ],
            expected: vec!["alpha", "charlie"],
        },
        SelectorCase {
            case: "the tenant-database-resource-id filter",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "tenant-database-resource-id",
                "Filters.Filter.1.Values.Value.1" => alpha_resource_id.clone(),
            ],
            expected: vec!["alpha"],
        },
        SelectorCase {
            case: "the dbi-resource-id filter",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "dbi-resource-id",
                "Filters.Filter.1.Values.Value.1" => second_dbi.clone(),
            ],
            expected: vec!["charlie"],
        },
        SelectorCase {
            case: "the db-instance-id filter, by identifier",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "db-instance-id",
                "Filters.Filter.1.Values.Value.1" => "emu-sel-a",
            ],
            expected: vec!["alpha", "bravo"],
        },
        SelectorCase {
            // "Accepts DB instance identifiers and DB instance Amazon Resource Names
            // (ARNs)."
            case: "the db-instance-id filter, by ARN",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "db-instance-id",
                "Filters.Filter.1.Values.Value.1" =>
                    format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-sel-a"),
            ],
            expected: vec!["alpha", "bravo"],
        },
        SelectorCase {
            case: "the db-snapshot-id filter, by identifier",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" => "emu-sel-snap-b",
            ],
            expected: vec!["charlie"],
        },
        SelectorCase {
            case: "the db-snapshot-id filter, by ARN",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" =>
                    format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:emu-sel-snap-b"),
            ],
            expected: vec!["charlie"],
        },
        SelectorCase {
            case: "the snapshot-type filter",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "snapshot-type",
                "Filters.Filter.1.Values.Value.1" => "manual",
            ],
            expected: vec!["alpha", "bravo", "charlie"],
        },
        SelectorCase {
            case: "two filters, each narrowing the other",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" => "emu-sel-snap-a",
                "Filters.Filter.2.Name" => "tenant-db-name",
                "Filters.Filter.2.Values.Value.1" => "bravo",
            ],
            expected: vec!["bravo"],
        },
        SelectorCase {
            case: "a selector and a filter that cannot both hold",
            parameters: owned_params![
                "DBSnapshotIdentifier" => "emu-sel-snap-a",
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "charlie",
            ],
            expected: Vec::new(),
        },
        SelectorCase {
            case: "a tenant name no tenant database carries",
            parameters: owned_params![
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "delta",
            ],
            expected: Vec::new(),
        },
    ];

    for row in table {
        let body = describe(&router, row.case, &row.parameters).await;
        assert_eq!(
            reported_names(&body),
            row.expected,
            "case {:?}: expected {:?}, got {:?} in {body}",
            row.case,
            row.expected,
            reported_names(&body)
        );
    }
}

#[tokio::test]
async fn a_snapshot_reports_what_it_captured_even_after_its_instance_changes() {
    let router = emulator();
    create_available_instance(&router, "emu-hist", "oracle-ee-cdb").await;
    for name in ["kept", "removed"] {
        create_tenant(
            &router,
            "emu-hist",
            &Tenant {
                name,
                master_username: TENANT_MASTER_USERNAME,
                character_set: None,
                nchar_character_set: None,
            },
        )
        .await;
    }
    take_available_snapshot(&router, "emu-hist", "emu-hist-snap").await;

    // The copy of a snapshot carries the tenant databases the source captured, under the
    // copy's own identifier and ARN.
    expect_ok(
        "copy the snapshot",
        call(
            &router,
            "CopyDBSnapshot",
            &params![
                "SourceDBSnapshotIdentifier" => "emu-hist-snap",
                "TargetDBSnapshotIdentifier" => "emu-hist-copy",
            ],
        )
        .await,
    );

    // The instance moves on: one more tenant database is created and one of the captured
    // ones is deleted.
    create_tenant(
        &router,
        "emu-hist",
        &Tenant {
            name: "added",
            master_username: TENANT_MASTER_USERNAME,
            character_set: None,
            nchar_character_set: None,
        },
    )
    .await;
    let read_added = params![
        "DBInstanceIdentifier" => "emu-hist",
        "TenantDBName" => "added",
    ];
    poll_until(
        "the added tenant database",
        "the tenant database reaching available",
        || call(&router, "DescribeTenantDatabases", &read_added),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some("available"),
    )
    .await;
    expect_ok(
        "delete one of the captured tenant databases",
        call(
            &router,
            "DeleteTenantDatabase",
            &params![
                "DBInstanceIdentifier" => "emu-hist",
                "TenantDBName" => "removed",
                "SkipFinalSnapshot" => "true",
            ],
        )
        .await,
    );

    for (case, snapshot) in [
        ("the snapshot itself", "emu-hist-snap"),
        ("the copy of the snapshot", "emu-hist-copy"),
    ] {
        let body = describe(&router, case, &params!["DBSnapshotIdentifier" => snapshot]).await;
        assert_eq!(
            reported_names(&body),
            vec!["kept".to_string(), "removed".to_string()],
            "case {case:?}: a snapshot reports the tenant databases its instance held when \
             it was taken, not the ones it holds now: {body}"
        );
        assert_eq!(
            text(&the_reported(case, &body, "kept"), "DBSnapshotIdentifier"),
            snapshot,
            "case {case:?}: each record names the snapshot that holds it: {body}"
        );
        assert_eq!(
            text(
                &the_reported(case, &body, "kept"),
                "DBSnapshotTenantDatabaseARN"
            ),
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot-tenant-database:{snapshot}:kept"),
            "case {case:?}: each record carries the ARN of the snapshot that holds it: {body}"
        );
    }
}

#[tokio::test]
async fn the_report_refuses_what_aws_refuses() {
    let router = emulator();
    create_available_instance(&router, "emu-ref", "oracle-ee-cdb").await;
    create_tenant(
        &router,
        "emu-ref",
        &Tenant {
            name: "tenant1",
            master_username: TENANT_MASTER_USERNAME,
            character_set: None,
            nchar_character_set: None,
        },
    )
    .await;
    take_available_snapshot(&router, "emu-ref", "emu-ref-snap").await;
    // A second multi-tenant instance with a snapshot of its own, so a request can name
    // resources that both exist and still name no snapshot; and a single-tenant instance,
    // whose snapshot is no subject of this report at all.
    create_available_instance(&router, "emu-ref-other", "oracle-ee-cdb").await;
    take_available_snapshot(&router, "emu-ref-other", "emu-ref-other-snap").await;
    create_available_instance(&router, "emu-ref-single", "postgres").await;
    take_available_snapshot(&router, "emu-ref-single", "emu-ref-single-snap").await;
    // Two more single-tenant engines, so the rows below span the configuration rather than
    // one engine: a MySQL-family instance, and an ORACLE instance that is not a container
    // database — the engine this command is about, in the configuration it is not about.
    create_available_instance(&router, "emu-ref-maria", "mariadb").await;
    take_available_snapshot(&router, "emu-ref-maria", "emu-ref-maria-snap").await;
    create_available_instance(&router, "emu-ref-noncdb", "oracle-ee").await;
    take_available_snapshot(&router, "emu-ref-noncdb", "emu-ref-noncdb-snap").await;

    // A request that names a DB SNAPSHOT this report resolves none of is the one fault this
    // report models, a 404 `DBSnapshotNotFound` — the only error the operation declares. The
    // rows differ along what the rule generalises over: whether the identifier names no
    // snapshot at all or names one taken off an instance outside the multi-tenant
    // configuration this command "only applies to" (three such engines, including Oracle
    // itself without a container database), and what else the request narrows by beside it.
    for (case, parameters, message) in [
        (
            "a snapshot that does not exist",
            owned_params!["DBSnapshotIdentifier" => "emu-ref-missing"],
            "DBSnapshot emu-ref-missing not found.",
        ),
        (
            "an existing snapshot of a single-tenant postgres instance",
            owned_params!["DBSnapshotIdentifier" => "emu-ref-single-snap"],
            "DBSnapshot emu-ref-single-snap not found.",
        ),
        (
            "an existing snapshot of a single-tenant mariadb instance",
            owned_params!["DBSnapshotIdentifier" => "emu-ref-maria-snap"],
            "DBSnapshot emu-ref-maria-snap not found.",
        ),
        (
            "an existing snapshot of an Oracle instance that is no container database",
            owned_params!["DBSnapshotIdentifier" => "emu-ref-noncdb-snap"],
            "DBSnapshot emu-ref-noncdb-snap not found.",
        ),
        (
            "that same snapshot, alongside the DB instance it was taken of",
            owned_params![
                "DBSnapshotIdentifier" => "emu-ref-noncdb-snap",
                "DBInstanceIdentifier" => "emu-ref-noncdb",
            ],
            "DBSnapshot emu-ref-noncdb-snap not found.",
        ),
        (
            "a snapshot that does not exist, alongside a snapshot type",
            owned_params![
                "DBSnapshotIdentifier" => "emu-ref-missing",
                "SnapshotType" => "manual",
            ],
            "DBSnapshot emu-ref-missing not found.",
        ),
        (
            "a snapshot that does not exist, alongside a DB instance that does",
            owned_params![
                "DBSnapshotIdentifier" => "emu-ref-missing",
                "DBInstanceIdentifier" => "emu-ref",
            ],
            "DBSnapshot emu-ref-missing not found.",
        ),
    ] {
        let body = expect_error(
            case,
            call(&router, "DescribeDBSnapshotTenantDatabases", &parameters).await,
            404,
            "DBSnapshotNotFound",
        );
        assert_eq!(
            text(&body, "Message"),
            message,
            "case {case:?}: the fault names the resource the request asked for: {body}"
        );
    }
    // A request that names NO DB snapshot is served rather than refused, whatever else it
    // narrows by: a snapshot type nothing was taken under, a tenant name nothing carries, an
    // id filter or a DB resource id nothing matches, a DB instance that exists but whose
    // snapshots this report has no subject in, and a multi-tenant snapshot that captured no
    // tenant database.
    for (case, parameters) in [
        (
            "a snapshot type no snapshot was taken under",
            owned_params!["SnapshotType" => "automated"],
        ),
        (
            "a tenant name no tenant database carries",
            owned_params![
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "nosuchtenant",
            ],
        ),
        (
            "a DB resource id no snapshot was taken from",
            owned_params!["DbiResourceId" => "db-EMUNOSUCHDBRESOURCEIDXXXXX"],
        ),
        (
            "the db-instance-id filter naming an instance no snapshot was taken of",
            owned_params![
                "Filters.Filter.1.Name" => "db-instance-id",
                "Filters.Filter.1.Values.Value.1" => "emu-ref-missing",
            ],
        ),
        (
            "the single-tenant instance a snapshot was taken of",
            owned_params!["DBInstanceIdentifier" => "emu-ref-single"],
        ),
        (
            "a multi-tenant snapshot that captured no tenant database",
            owned_params!["DBSnapshotIdentifier" => "emu-ref-other-snap"],
        ),
    ] {
        let body = describe(&router, case, &parameters).await;
        assert!(
            reported_names(&body).is_empty(),
            "case {case:?}: expected an empty report, got {:?} in {body}",
            reported_names(&body)
        );
    }

    // Every other refusal is a 400 `InvalidParameterValue`: the rows differ along what the
    // rule generalises over — an unsupported filter name, each published `MaxRecords` bound
    // and one value past it, and a pagination token this report's codec never produced.
    for (case, parameters, refused) in [
        (
            "a filter name this operation does not support",
            owned_params![
                "Filters.Filter.1.Name" => "tenant-name",
                "Filters.Filter.1.Values.Value.1" => "tenant1",
            ],
            true,
        ),
        (
            "MaxRecords one below the documented minimum",
            owned_params!["MaxRecords" => "19"],
            true,
        ),
        (
            "MaxRecords at the documented minimum",
            owned_params!["MaxRecords" => "20"],
            false,
        ),
        (
            "MaxRecords at the documented maximum",
            owned_params!["MaxRecords" => "100"],
            false,
        ),
        (
            "MaxRecords one above the documented maximum",
            owned_params!["MaxRecords" => "101"],
            true,
        ),
        (
            "a marker this report could not have issued",
            owned_params!["Marker" => "not-a-marker"],
            true,
        ),
    ] {
        let response = call(
            &router,
            "DescribeDBSnapshotTenantDatabases",
            &parameters,
        )
        .await;
        if refused {
            expect_error(case, response, 400, "InvalidParameterValue");
        } else {
            let body = expect_ok(case, response);
            assert_eq!(
                reported_names(&body),
                vec!["tenant1".to_string()],
                "case {case:?}: a value inside the documented window is served: {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_report_longer_than_one_page_walks_through_its_marker() {
    let router = emulator();
    // More tenant databases than one page holds, spread over as many instances as the
    // per-instance quota needs: `MaxRecords` is "Minimum 20, maximum 100", so a second page
    // needs more than twenty records.
    let instances = ["emu-page-a", "emu-page-b", "emu-page-c"];
    let tenants_per_instance = [10usize, 10, 1];
    for instance in instances {
        expect_ok(
            &format!("create instance {instance}"),
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => instance,
                    "Engine" => "oracle-ee-cdb",
                    "DBInstanceClass" => INSTANCE_CLASS,
                ],
            )
            .await,
        );
    }
    let mut expected: Vec<String> = Vec::new();
    for (instance, count) in instances.iter().zip(tenants_per_instance) {
        let read = params!["DBInstanceIdentifier" => instance];
        poll_until(
            instance,
            "the DB instance reaching available",
            || call(&router, "DescribeDBInstances", &read),
            |status, body| {
                status == 200
                    && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
            },
        )
        .await;
        for index in 0..count {
            let name = format!("t{index:02}");
            let mut parameters = owned_params![
                "DBInstanceIdentifier" => instance,
                "MasterUsername" => TENANT_MASTER_USERNAME,
            ];
            parameters.push(("TenantDBName", name.clone()));
            expect_ok(
                &format!("create tenant database {name} in {instance}"),
                call(&router, "CreateTenantDatabase", &parameters).await,
            );
            expected.push(name);
        }
        take_available_snapshot(&router, instance, &format!("{instance}-snap")).await;
    }
    expected.sort();

    let page_size = 20usize;
    let mut reported: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut parameters = owned_params!["MaxRecords" => page_size];
        if let Some(token) = &marker {
            parameters.push(("Marker", token.clone()));
        }
        let case = format!("page {} of the report", pages + 1);
        let body = describe(&router, &case, &parameters).await;
        let names = reported_names(&body);
        assert!(
            names.len() <= page_size,
            "case {case:?}: a page holds at most MaxRecords records, got {}: {body}",
            names.len()
        );
        reported.extend(names);
        pages += 1;
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            break;
        }
        assert!(
            pages <= expected.len(),
            "the walk must end: {pages} pages for {} records",
            expected.len()
        );
    }
    assert!(
        pages > 1,
        "a report of {} records cannot fit in one page of {page_size}: it took {pages}",
        expected.len()
    );
    reported.sort();
    assert_eq!(
        reported, expected,
        "the pages together report every captured tenant database exactly once, expected \
         {expected:?}, got {reported:?}"
    );
}

#[tokio::test]
async fn a_selection_that_carries_no_tenant_database_is_an_empty_report() {
    // The rule: `DBSnapshotNotFoundFault` — the one error this operation models — answers
    // one question only, whether this service holds a DB snapshot of a MULTI-TENANT instance
    // (the configuration the command "only applies to") under the identifier the request
    // named, so every other narrowing the request carries selects which rows the report
    // carries, and a selection that comes out with none is a 200 whose
    // `DBSnapshotTenantDatabases` list is empty and which carries no `Marker`.
    //
    // The rows below span what that generalises over: which narrowing empties the report (a
    // snapshot that captured nothing, a single-tenant engine the command "only applies" past
    // — reached through a FILTER, since naming such a snapshot as the subject is the fault
    // above — the source DB instance, one this service holds and one it does not, the DB
    // resource id, the snapshot type, each documented filter name), whether the resource is
    // named by the selector, by the filter's identifier form or by the filter's ARN form, the
    // case the instance identifier is spelled in, and a filter value that names no snapshot
    // at all.
    let router = emulator();
    create_available_instance(&router, "emu-nil-cdb", "oracle-ee-cdb").await;
    create_tenant(
        &router,
        "emu-nil-cdb",
        &Tenant {
            name: "tenant1",
            master_username: TENANT_MASTER_USERNAME,
            character_set: None,
            nchar_character_set: None,
        },
    )
    .await;
    take_available_snapshot(&router, "emu-nil-cdb", "emu-nil-cdb-snap").await;
    // A multi-tenant instance that holds no tenant database when its snapshot is taken, so
    // the snapshot exists and captured nothing.
    create_available_instance(&router, "emu-nil-bare", "oracle-se2-cdb").await;
    take_available_snapshot(&router, "emu-nil-bare", "emu-nil-bare-snap").await;
    // A single-tenant engine: the configuration this command does not apply to.
    create_available_instance(&router, "emu-nil-single", "postgres").await;
    take_available_snapshot(&router, "emu-nil-single", "emu-nil-single-snap").await;
    // A second multi-tenant instance, so a request can name two resources that both exist
    // and still select nothing.
    let other_resource_id =
        create_available_instance(&router, "emu-nil-other", "oracle-ee-cdb").await;
    take_available_snapshot(&router, "emu-nil-other", "emu-nil-other-snap").await;
    let single_snapshot_arn =
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:emu-nil-single-snap");

    for (case, parameters) in [
        (
            "a multi-tenant snapshot that captured no tenant database",
            owned_params!["DBSnapshotIdentifier" => "emu-nil-bare-snap"],
        ),
        (
            "a snapshot taken off a single-tenant engine, named by the db-snapshot-id filter",
            owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" => "emu-nil-single-snap",
            ],
        ),
        (
            "a snapshot taken off a single-tenant engine, named by its ARN in that filter",
            owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" => single_snapshot_arn,
            ],
        ),
        (
            "a snapshot that exists, under a type it was not taken with",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "SnapshotType" => "automated",
            ],
        ),
        (
            "a snapshot that exists, under a DB resource id it was not taken from",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "DbiResourceId" => other_resource_id,
            ],
        ),
        (
            "a snapshot that exists, under another instance that exists too",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "DBInstanceIdentifier" => "emu-nil-other",
            ],
        ),
        (
            "a snapshot that exists, under a tenant name none of its tenants carries",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "Filters.Filter.1.Name" => "tenant-db-name",
                "Filters.Filter.1.Values.Value.1" => "nosuchtenant",
            ],
        ),
        (
            "a snapshot that exists, under a tenant resource id none of its tenants carries",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "Filters.Filter.1.Name" => "tenant-database-resource-id",
                "Filters.Filter.1.Values.Value.1" => "tenant-EMUNOSUCHTENANTRESOURCEID",
            ],
        ),
        (
            "a snapshot that exists, under a dbi-resource-id filter it was not taken from",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "Filters.Filter.1.Name" => "dbi-resource-id",
                "Filters.Filter.1.Values.Value.1" => other_resource_id,
            ],
        ),
        (
            "a snapshot that exists, under a db-instance-id filter it was not taken of",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "Filters.Filter.1.Name" => "db-instance-id",
                "Filters.Filter.1.Values.Value.1" => "emu-nil-other",
            ],
        ),
        (
            "a snapshot that exists, under a snapshot-type filter it was not taken with",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "Filters.Filter.1.Name" => "snapshot-type",
                "Filters.Filter.1.Values.Value.1" => "automated",
            ],
        ),
        (
            "the db-snapshot-id filter naming a snapshot this service holds none of",
            owned_params![
                "Filters.Filter.1.Name" => "db-snapshot-id",
                "Filters.Filter.1.Values.Value.1" => "emu-nil-missing",
            ],
        ),
        (
            "a DB instance this service holds none of",
            owned_params!["DBInstanceIdentifier" => "emu-nil-missing"],
        ),
        (
            "a DB instance this service holds none of, spelled in another case",
            owned_params!["DBInstanceIdentifier" => "EMU-NIL-MISSING"],
        ),
        (
            "a DB instance this service holds none of, alongside a snapshot that exists",
            owned_params![
                "DBSnapshotIdentifier" => "emu-nil-cdb-snap",
                "DBInstanceIdentifier" => "emu-nil-missing",
            ],
        ),
        (
            "the db-instance-id filter naming an instance this service holds none of",
            owned_params![
                "Filters.Filter.1.Name" => "db-instance-id",
                "Filters.Filter.1.Values.Value.1" => "emu-nil-missing",
            ],
        ),
    ] {
        let body = describe(&router, case, &parameters).await;
        assert!(
            reported_names(&body).is_empty(),
            "case {case:?}: expected an empty report, got {:?} in {body}",
            reported_names(&body)
        );
        // The success envelope carries the modelled list member, EMPTY, rather than leaving
        // it out: the report selected nothing, it did not fail to report.
        assert!(
            body.contains("<DBSnapshotTenantDatabases/>")
                || body.contains("<DBSnapshotTenantDatabases></DBSnapshotTenantDatabases>"),
            "case {case:?}: the result carries an empty DBSnapshotTenantDatabases: {body}"
        );
        assert!(
            optional_text(&body, "Marker").is_none(),
            "case {case:?}: a report with nothing left to walk carries no Marker: {body}"
        );
    }

    // And the same filter NARROWS rather than refuses: carrying one snapshot this service
    // holds and one it does not reports the tenant databases of the one it holds.
    let body = describe(
        &router,
        "the db-snapshot-id filter carrying a snapshot that exists and one that does not",
        &params![
            "Filters.Filter.1.Name" => "db-snapshot-id",
            "Filters.Filter.1.Values.Value.1" => "emu-nil-cdb-snap",
            "Filters.Filter.1.Values.Value.2" => "emu-nil-missing",
        ],
    )
    .await;
    assert_eq!(
        reported_names(&body),
        vec!["tenant1".to_string()],
        "a filter value that names no snapshot narrows the report instead of refusing it: \
         {body}"
    );
}
