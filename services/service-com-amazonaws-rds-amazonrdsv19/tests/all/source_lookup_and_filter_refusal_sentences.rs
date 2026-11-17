// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the sentences this front door answers a LOOKUP that resolved to
//! nothing, an identifier of an unusable shape, and a filter name it does not publish with.
//!
//! The rules under test, each one level above the call that exposed it:
//!
//! * A copy whose SOURCE parameter group does not exist is refused with the one
//!   parameter-less sentence the copy states, naming neither the group nor the member — the
//!   cluster-level copy and the instance-level copy alike, and whether the source came as a
//!   name or as an ARN.
//! * A refusal naming the resource an operation's single SOURCE member points at is worded
//!   about that source: the fixed framing names the member's role and the KIND of resource
//!   the value named, and brackets the value exactly as the request passed it — not the
//!   sentence a call whose own SUBJECT is that resource answers. The read replica's source
//!   DB instance is the same rule on a request that carries TWO identifiers: the refusal
//!   says the SOURCE was not found and carries the value `SourceDBInstanceIdentifier`
//!   brought, while the generic instance lookup keeps its own sentence.
//! * A state precondition about that source is the service's own FIXED sentence: the
//!   read-replica create states the state a source has to be in and names neither the
//!   source the request carried nor the status it is in.
//! * An operation that resolves a resource it takes no identifier of its own for words the
//!   not-found fault its own way; the resource type comes first and the request's identifier
//!   follows the fixed phrase.
//! * An identifier whose SHAPE is not one the service assigns is refused before any lookup,
//!   with a sentence stating that the identifier is not one, carrying the value as sent and
//!   nothing else — no member name and no account of the form the service assigns.
//! * The one report whose identifier member the model documents as not case-sensitive quotes
//!   the identifier FOLDED in its not-found fault, because the folded value is the handle it
//!   looked up; a member resolved as sent is quoted as sent.
//! * A `Filters` entry whose `Name` is not one the operation publishes is refused with fixed
//!   text and the offending name, and nothing after it — the same sentence from every report
//!   of this front door, whether it publishes several filter names or none at all.
//!
//! Each table spans what its rule generalises over: which copy carried the missing source and
//! whether it was named or ARN-addressed; which kind of database the blue/green source ARN
//! named; the shape of the identifier the sentence carries; which way an identifier fails its
//! shape; the case the identifier was sent in; and which report, with which unpublished name,
//! refused the filter. The recorded request is ONE row of each.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, text, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The Query API's own code for a member value the service will not serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// `DBParameterGroupNotFoundFault`'s `awsQueryError` code
/// (`smithy_query(query="shape", shape="DBParameterGroupNotFoundFault")`).
const DB_PARAMETER_GROUP_NOT_FOUND: &str = "DBParameterGroupNotFound";

/// `DBInstanceNotFoundFault`'s `awsQueryError` code.
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// `InvalidDBInstanceStateFault`'s `awsQueryError` code.
const INVALID_DB_INSTANCE_STATE: &str = "InvalidDBInstanceState";

/// `DBClusterNotFoundFault`'s `awsQueryError` code.
const DB_CLUSTER_NOT_FOUND: &str = "DBClusterNotFoundFault";

/// `BlueGreenDeploymentNotFoundFault`'s `awsQueryError` code.
const BLUE_GREEN_DEPLOYMENT_NOT_FOUND: &str = "BlueGreenDeploymentNotFoundFault";

/// `DBClusterAutomatedBackupNotFoundFault`'s `awsQueryError` code.
const DB_CLUSTER_AUTOMATED_BACKUP_NOT_FOUND: &str = "DBClusterAutomatedBackupNotFoundFault";

/// An RDS resource ARN of `resource_type` in the region the harness signs for.
fn resource_arn(resource_type: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:{resource_type}:{identifier}")
}

// ---------------------------------------------------------------------------
// A copy whose source parameter group is not there
// ---------------------------------------------------------------------------

/// One row of the copy table: the copy operation, the member it names its source by, the
/// member it names the new group by, the member carrying the new group's description, and the
/// source value that resolves to nothing.
struct CopyCase {
    case: &'static str,
    action: &'static str,
    source_member: &'static str,
    target_member: &'static str,
    description_member: &'static str,
    source: String,
    target: &'static str,
}

#[tokio::test]
async fn a_parameter_group_copy_whose_source_is_missing_states_the_one_fixed_sentence() {
    let router = emulator();
    // The sentence has NO variable part: it names neither the group the request could not
    // find nor the member it named it by, so every row below expects the same string.
    let expected = "Parameter group does not exist";

    let rows = vec![
        CopyCase {
            case: "a cluster-level copy from a name that exists nowhere",
            action: "CopyDBClusterParameterGroup",
            source_member: "SourceDBClusterParameterGroupIdentifier",
            target_member: "TargetDBClusterParameterGroupIdentifier",
            description_member: "TargetDBClusterParameterGroupDescription",
            source: "emu-src-cpg-absent".to_string(),
            target: "emu-copy-cpg-1",
        },
        CopyCase {
            case: "a cluster-level copy from a digit-bearing name",
            action: "CopyDBClusterParameterGroup",
            source_member: "SourceDBClusterParameterGroupIdentifier",
            target_member: "TargetDBClusterParameterGroupIdentifier",
            description_member: "TargetDBClusterParameterGroupDescription",
            source: "emu-src-cpg-8-0".to_string(),
            target: "emu-copy-cpg-2",
        },
        CopyCase {
            case: "a cluster-level copy from an ARN of a group that exists nowhere",
            action: "CopyDBClusterParameterGroup",
            source_member: "SourceDBClusterParameterGroupIdentifier",
            target_member: "TargetDBClusterParameterGroupIdentifier",
            description_member: "TargetDBClusterParameterGroupDescription",
            source: resource_arn("cluster-pg", "emu-src-cpg-by-arn"),
            target: "emu-copy-cpg-3",
        },
        CopyCase {
            case: "an instance-level copy from a name that exists nowhere",
            action: "CopyDBParameterGroup",
            source_member: "SourceDBParameterGroupIdentifier",
            target_member: "TargetDBParameterGroupIdentifier",
            description_member: "TargetDBParameterGroupDescription",
            source: "emu-src-pg-absent".to_string(),
            target: "emu-copy-pg-1",
        },
        CopyCase {
            case: "an instance-level copy from an ARN of a group that exists nowhere",
            action: "CopyDBParameterGroup",
            source_member: "SourceDBParameterGroupIdentifier",
            target_member: "TargetDBParameterGroupIdentifier",
            description_member: "TargetDBParameterGroupDescription",
            source: resource_arn("pg", "emu-src-pg-by-arn"),
            target: "emu-copy-pg-2",
        },
    ];

    for row in &rows {
        let params = [
            (row.source_member, row.source.clone()),
            (row.target_member, row.target.to_string()),
            (row.description_member, "a copy of a missing group".to_string()),
        ];
        let body = expect_error(
            row.case,
            call(&router, row.action, &params).await,
            404,
            DB_PARAMETER_GROUP_NOT_FOUND,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
        // The identifier the request carried is NOT reported back: the sentence is the
        // service's own statement that the group is not there.
        assert!(
            !message.contains(row.source.as_str()),
            "case {:?}: the sentence must not report the source back, got {message:?}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// A blue/green deployment whose SOURCE database is not there
// ---------------------------------------------------------------------------

/// One row of the blue/green source table: the ARN resource type `Source` carried, the
/// identifier inside it, how the sentence names that kind of database, and the fault code the
/// kind decides.
struct SourceCase {
    case: &'static str,
    resource_type: &'static str,
    identifier: &'static str,
    subject: &'static str,
    code: &'static str,
}

#[tokio::test]
async fn a_blue_green_deployment_frames_a_missing_source_database_as_the_source() {
    let router = emulator();
    let rows = [
        SourceCase {
            case: "a DB instance ARN naming no instance",
            resource_type: "db",
            identifier: "emu-bgd-src-absent",
            subject: "DB instance",
            code: DB_INSTANCE_NOT_FOUND,
        },
        SourceCase {
            case: "a DB instance ARN whose identifier carries digits",
            resource_type: "db",
            identifier: "emu-bgd-src-17",
            subject: "DB instance",
            code: DB_INSTANCE_NOT_FOUND,
        },
        SourceCase {
            case: "a DB instance ARN whose identifier is a single letter",
            resource_type: "db",
            identifier: "e",
            subject: "DB instance",
            code: DB_INSTANCE_NOT_FOUND,
        },
        SourceCase {
            case: "a DB cluster ARN naming no cluster",
            resource_type: "cluster",
            identifier: "emu-bgd-src-cluster-absent",
            subject: "DB cluster",
            code: DB_CLUSTER_NOT_FOUND,
        },
        SourceCase {
            case: "a DB cluster ARN whose identifier carries digits",
            resource_type: "cluster",
            identifier: "emu-bgd-src-cluster-9",
            subject: "DB cluster",
            code: DB_CLUSTER_NOT_FOUND,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let source = resource_arn(row.resource_type, row.identifier);
        let body = expect_error(
            row.case,
            call(
                &router,
                "CreateBlueGreenDeployment",
                &[
                    ("BlueGreenDeploymentName", format!("emu-bgd-name-{index}")),
                    ("Source", source.clone()),
                ],
            )
            .await,
            404,
            row.code,
        );
        let message = text(&body, "Message");
        let expected = format!("Source {} [{source}] not found", row.subject);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }

    // A `Source` that is no ARN at all names no database to look for, so it is a value the
    // member does not accept rather than a database that is missing: the refusal is NOT the
    // not-found fault.
    let body = expect_error(
        "a Source that is not an ARN at all",
        call(
            &router,
            "CreateBlueGreenDeployment",
            &[
                ("BlueGreenDeploymentName", "emu-bgd-name-shape".to_string()),
                ("Source", "not-an-arn-at-all".to_string()),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let code = text(&body, "Code");
    assert_ne!(
        code, DB_INSTANCE_NOT_FOUND,
        "case \"a Source that is not an ARN at all\": a value that names no database must \
         not be answered as a missing one, got {code:?}"
    );
}

// ---------------------------------------------------------------------------
// A tenant database on a DB instance that is not there
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creating_a_tenant_database_names_the_missing_instance_after_the_fixed_phrase() {
    let router = emulator();
    // The identifier shapes this member accepts: plain, digit-bearing, a single letter, and
    // one carrying several hyphens.
    let rows = [
        ("a plain identifier", "emu-tdb-absent"),
        ("a digit-bearing identifier", "emu-tdb-absent-42"),
        ("a single-letter identifier", "t"),
        ("a multi-hyphen identifier", "emu-tdb-a-b-c-absent"),
    ];

    for (case, identifier) in rows {
        let body = expect_error(
            case,
            call(
                &router,
                "CreateTenantDatabase",
                &[
                    ("DBInstanceIdentifier", identifier),
                    ("TenantDBName", "tenantone"),
                    ("MasterUsername", "tenantadmin"),
                    ("MasterUserPassword", "a-tenant-password-1"),
                ],
            )
            .await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = format!("DBInstance not found: {identifier}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A blue/green deployment identifier of an unusable shape, and of an unknown one
// ---------------------------------------------------------------------------

/// A well-shaped blue/green deployment identifier naming nothing: the prefix the service
/// assigns plus `token`.
fn well_shaped(token: &str) -> String {
    format!("bgd-{token}")
}

#[tokio::test]
async fn an_ill_shaped_blue_green_deployment_identifier_states_only_the_value() {
    let router = emulator();
    // Four ways a value fails the shape the service assigns: it carries no prefix at all,
    // it stops at the prefix's letters, it carries the prefix with an empty token, and it is
    // an ARN whose resource part is not of the assigned form.
    let rows = [
        ("an arbitrary string", "some-arbitrary-string".to_string()),
        ("the prefix letters alone", "bgd".to_string()),
        ("the prefix with an empty token", "bgd-".to_string()),
        (
            "an ARN whose resource part is not of the assigned form",
            resource_arn("blue-green-deployment", "not-of-the-assigned-form"),
        ),
    ];

    for (case, value) in &rows {
        let body = expect_error(
            case,
            call(
                &router,
                "DeleteBlueGreenDeployment",
                &[("BlueGreenDeploymentIdentifier", value.clone())],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!("Invalid blue-green deployment identifier: {value}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_delete_of_an_unknown_deployment_quotes_the_identifier_as_sent() {
    let router = emulator();
    // The same well-shaped identifier in three cases: the member is resolved as the request
    // sent it here, so the sentence quotes exactly that.
    let rows = [
        ("a lower-case token", well_shaped("0123456789abcdef")),
        ("an upper-case token", well_shaped("0123456789ABCDEF")),
        ("a mixed-case token", well_shaped("0123456789AbCdEf")),
    ];

    for (case, value) in &rows {
        let body = expect_error(
            case,
            call(
                &router,
                "DeleteBlueGreenDeployment",
                &[("BlueGreenDeploymentIdentifier", value.clone())],
            )
            .await,
            404,
            BLUE_GREEN_DEPLOYMENT_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = format!("Blue Green Deployment '{value}' doesn't exist.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_report_of_an_unknown_deployment_quotes_the_identifier_folded() {
    let router = emulator();
    // "This parameter isn't case-sensitive" (`DescribeBlueGreenDeployments`'
    // `BlueGreenDeploymentIdentifier`), so the handle looked up — and quoted back — is the
    // folded one, whichever case the request sent.
    let rows = [
        ("a lower-case token", well_shaped("abcdef0123456789")),
        ("an upper-case token", well_shaped("ABCDEF0123456789")),
        ("a mixed-case token", well_shaped("AbCdEf0123456789")),
        ("an upper-case 26-character token", well_shaped("73F3B5C1EFE04A64BD2719E152")),
    ];

    for (case, value) in &rows {
        let body = expect_error(
            case,
            call(
                &router,
                "DescribeBlueGreenDeployments",
                &[("BlueGreenDeploymentIdentifier", value.clone())],
            )
            .await,
            404,
            BLUE_GREEN_DEPLOYMENT_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = format!(
            "Blue Green Deployment '{}' doesn't exist.",
            value.to_ascii_lowercase()
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The two calls differ exactly in this: the case-insensitive report folds the handle it
    // quotes, the delete quotes what it was sent.
    let sent = well_shaped("FEDCBA9876543210");
    let reported = text(
        &expect_error(
            "the report of an upper-case identifier",
            call(
                &router,
                "DescribeBlueGreenDeployments",
                &[("BlueGreenDeploymentIdentifier", sent.clone())],
            )
            .await,
            404,
            BLUE_GREEN_DEPLOYMENT_NOT_FOUND,
        ),
        "Message",
    );
    let deleted = text(
        &expect_error(
            "the delete of an upper-case identifier",
            call(
                &router,
                "DeleteBlueGreenDeployment",
                &[("BlueGreenDeploymentIdentifier", sent.clone())],
            )
            .await,
            404,
            BLUE_GREEN_DEPLOYMENT_NOT_FOUND,
        ),
        "Message",
    );
    assert_ne!(
        reported, deleted,
        "case \"an upper-case identifier\": the case-insensitive report folds the identifier \
         it quotes and the delete does not, got {reported:?} from both"
    );
    assert!(
        reported.contains(&sent.to_ascii_lowercase()) && deleted.contains(sent.as_str()),
        "case \"an upper-case identifier\": expected the report to quote \
         {:?} and the delete to quote {sent:?}, got {reported:?} and {deleted:?}",
        sent.to_ascii_lowercase()
    );
}

// ---------------------------------------------------------------------------
// An automated backup for a DB cluster that is not there
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_cluster_automated_backup_names_both_modelled_resource_types() {
    let router = emulator();
    let rows = [
        ("a plain cluster identifier", "emu-bak-absent"),
        ("a digit-bearing cluster identifier", "emu-bak-absent-3"),
        ("a single-letter cluster identifier", "c"),
        ("a multi-hyphen cluster identifier", "emu-bak-a-b-absent"),
    ];

    for (case, identifier) in rows {
        let body = expect_error(
            case,
            call(
                &router,
                "DescribeDBClusterAutomatedBackups",
                &[("DBClusterIdentifier", identifier)],
            )
            .await,
            404,
            DB_CLUSTER_AUTOMATED_BACKUP_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = format!("DBClusterAutomatedBackup for DBCluster {identifier} not found.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A filter name the report does not publish
// ---------------------------------------------------------------------------

/// One row of the filter table: the report, and the `Filter$Name` it does not publish.
struct FilterCase {
    case: &'static str,
    action: &'static str,
    name: &'static str,
}

#[tokio::test]
async fn an_unpublished_filter_name_is_refused_with_fixed_text_and_the_name() {
    let router = emulator();
    let rows = [
        FilterCase {
            case: "a name no report of this service publishes",
            action: "DescribePendingMaintenanceActions",
            name: "not-a-filter-at-all",
        },
        FilterCase {
            case: "a case variant of a name this report does publish",
            action: "DescribePendingMaintenanceActions",
            name: "DB-Instance-Id",
        },
        FilterCase {
            case: "a name another report publishes",
            action: "DescribeDBClusters",
            name: "db-instance-id",
        },
        FilterCase {
            case: "a truncation of a name this report publishes",
            action: "DescribeBlueGreenDeployments",
            name: "blue-green-deployment",
        },
        FilterCase {
            case: "a report whose Filters member publishes no name at all",
            action: "DescribeDBShardGroups",
            name: "db-shard-group-id",
        },
        FilterCase {
            case: "a report whose Filters member is not supported",
            action: "DescribeDBProxies",
            name: "db-proxy-name",
        },
        FilterCase {
            case: "a report of integrations, which publishes no filter name",
            action: "DescribeIntegrations",
            name: "integration-arn",
        },
        FilterCase {
            case: "a case variant on the instance report",
            action: "DescribeDBInstances",
            name: "Engine",
        },
        FilterCase {
            case: "a case variant on the snapshot report",
            action: "DescribeDBSnapshots",
            name: "snapshot-Type",
        },
    ];

    for row in &rows {
        let body = expect_error(
            row.case,
            call(
                &router,
                row.action,
                &[
                    ("Filters.Filter.1.Name", row.name),
                    ("Filters.Filter.1.Values.Value.1", "any-value"),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!("Unrecognized filter name: {}", row.name);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// The SOURCE DB instance of a read replica: the lookup, and the state it has to be in
// ---------------------------------------------------------------------------

/// The sentence the read-replica create answers for a SOURCE DB instance it cannot resolve:
/// fixed text naming the SOURCE, then the value the request carried for it.
fn read_replica_source_not_found(named: &str) -> String {
    format!("The source instance could not be found: {named}")
}

/// The sentence a call whose single identifier names its SUBJECT answers for the same miss —
/// the generic instance lookup, which the source role must not reuse.
fn subject_instance_not_found(named: &str) -> String {
    format!("DBInstance not found: {named}")
}

/// The FIXED sentence the read-replica create answers for a source that is not `available`:
/// it names neither the source nor the status the source is in.
const SOURCE_NOT_AVAILABLE: &str = "DB instance is not in the available state";

/// `CreateDBInstance` with the automated backups a read replica's source needs ("its backup
/// retention period must be greater than 0"), so a refusal below is about the source's
/// existence or its status and never about its backups.
async fn create_replica_source(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", engine),
                ("DBInstanceClass", "db.t3.micro"),
                ("AllocatedStorage", "20"),
                ("BackupRetentionPeriod", "1"),
            ],
        )
        .await,
    );
}

/// `DescribeDBInstances` for one identifier in `region`, as the status and the body.
async fn read_instance(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await
}

/// Poll `DescribeDBInstances` until the instance reports `status` — the condition the next
/// call needs, rather than a fixed wait.
async fn poll_for_status(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) {
    poll_until(
        case,
        &format!("{identifier} reporting {status}"),
        || read_instance(router, REGION, identifier),
        |http, body| {
            http == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some(status)
        },
    )
    .await;
}

/// `CreateDBInstanceReadReplica` of `source` into `replica`, signed for `region`.
async fn create_read_replica(
    router: &MultiServiceRouter,
    region: &str,
    replica: &str,
    source: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBInstanceReadReplica",
        &[
            ("DBInstanceIdentifier", replica),
            ("SourceDBInstanceIdentifier", source),
        ],
    )
    .await
}

/// A `SourceDBInstanceIdentifier` of exactly the 63 characters a DB instance identifier
/// admits ("Must contain from 1 to 63 letters, numbers, or hyphens" —
/// `smithy_query(query="doc", shape="CreateDBInstanceMessage",
/// member="DBInstanceIdentifier")`), so the published upper bound of the value is a row.
fn identifier_at_the_published_bound() -> String {
    let mut identifier = String::from("e");
    identifier.extend(std::iter::repeat_n('a', 62));
    assert_eq!(
        identifier.len(),
        63,
        "the row at the published bound must be 63 characters, got {}",
        identifier.len()
    );
    identifier
}

/// One row of the source-lookup table: how the request names a source instance that is not
/// there, the replica it would have minted, and the region the call is signed for.
struct SourceInstanceCase {
    case: &'static str,
    replica: &'static str,
    named: String,
    region: &'static str,
}

#[tokio::test]
async fn a_read_replica_create_reports_the_source_instance_it_could_not_find() {
    let router = emulator();

    // One stored, running source, so every refusal below is a decision about the identifier
    // the request named rather than "this service holds no instance at all".
    create_replica_source(&router, "the stored source", "emu-src-role-held", "mysql").await;
    poll_for_status(&router, "the stored source", "emu-src-role-held", "available").await;
    // And one created and then deleted, so a row can name an identifier that resolved a
    // moment ago and resolves no longer.
    create_replica_source(&router, "the deleted source", "emu-src-role-gone", "postgres").await;
    expect_ok(
        "delete the source the table names",
        call(
            &router,
            "DeleteDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-src-role-gone"),
                ("SkipFinalSnapshot", "true"),
            ],
        )
        .await,
    );
    poll_until(
        "the deleted source",
        "emu-src-role-gone leaving the store",
        || read_instance(&router, REGION, "emu-src-role-gone"),
        |http, _| http == 404,
    )
    .await;

    // The rows differ along what the rule generalises over: the form the member carried (a
    // plain identifier, the same one upper-cased, the instance's ARN), the LENGTH of the
    // value (a single letter and the published 63-character bound), why the lookup resolves
    // nothing (never created, deleted a moment ago, held only by another region) and the
    // region the call is signed for.
    let rows = [
        SourceInstanceCase {
            case: "a source identifier nothing created",
            replica: "emu-src-role-replica-1",
            named: "emu-src-role-absent".to_string(),
            region: REGION,
        },
        SourceInstanceCase {
            case: "the same identifier upper-cased",
            replica: "emu-src-role-replica-2",
            named: "EMU-SRC-ROLE-ABSENT".to_string(),
            region: REGION,
        },
        SourceInstanceCase {
            case: "the ARN form of an identifier nothing created",
            replica: "emu-src-role-replica-3",
            named: resource_arn("db", "emu-src-role-absent"),
            region: REGION,
        },
        SourceInstanceCase {
            case: "a single-letter identifier",
            replica: "emu-src-role-replica-4",
            named: "e".to_string(),
            region: REGION,
        },
        SourceInstanceCase {
            case: "an identifier at the published 63-character bound",
            replica: "emu-src-role-replica-5",
            named: identifier_at_the_published_bound(),
            region: REGION,
        },
        SourceInstanceCase {
            case: "a source deleted a moment ago",
            replica: "emu-src-role-replica-6",
            named: "emu-src-role-gone".to_string(),
            region: REGION,
        },
        SourceInstanceCase {
            case: "the stored source named from another region",
            replica: "emu-src-role-replica-7",
            named: "emu-src-role-held".to_string(),
            region: "eu-central-1",
        },
    ];

    for row in &rows {
        let case = row.case;
        let body = expect_error(
            case,
            create_read_replica(&router, row.region, row.replica, &row.named).await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = read_replica_source_not_found(&row.named);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the source-lookup sentence {expected:?}, got {message:?}"
        );
        let subject = subject_instance_not_found(&row.named);
        assert_ne!(
            message, subject,
            "case {case:?}: the source lookup must not reuse the subject sentence {subject:?}"
        );
        // The refused create stored no replica under the identifier it would have minted.
        expect_error(
            case,
            read_instance(&router, row.region, row.replica).await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
    }

    // The same miss on a call whose SINGLE identifier names its subject: the generic
    // instance-lookup sentence, which is what makes the rule above about the ROLE the
    // identifier plays rather than about one house style.
    for named in ["emu-src-role-absent", "emu-src-role-gone"] {
        let case = format!("DescribeValidDBInstanceModifications on {named}");
        let body = expect_error(
            &case,
            call(
                &router,
                "DescribeValidDBInstanceModifications",
                &[("DBInstanceIdentifier", named)],
            )
            .await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = subject_instance_not_found(named);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the subject sentence {expected:?}, got {message:?}"
        );
    }

    // The stored source was there throughout, and a replica of it is served: the refusals
    // were about the lookup and nothing else.
    let served = expect_ok(
        "a replica of the stored source",
        create_read_replica(&router, REGION, "emu-src-role-replica-ok", "emu-src-role-held")
            .await,
    );
    assert_eq!(
        optional_text(&served, "ReadReplicaSourceDBInstanceIdentifier").as_deref(),
        Some("emu-src-role-held"),
        "case \"a replica of the stored source\": got {served}"
    );
}

/// One row of the source-state table: the engine the source runs, how the request names it,
/// and the replica it would have minted. Every row's source is STOPPED — a status an
/// instance stays in until a start is asked for, so the row's precondition is settled by
/// polling for it and carries no timing assumption.
struct SourceStateCase {
    case: &'static str,
    source: &'static str,
    replica: &'static str,
    engine: &'static str,
    /// Whether the request names the source by its ARN rather than by its identifier.
    by_arn: bool,
}

#[tokio::test]
async fn a_source_instance_that_is_not_available_is_refused_with_one_fixed_sentence() {
    let router = emulator();

    // The rows differ along what the sentence must NOT vary with: the engine the source
    // runs and the form the request named it by. The status is the one a test can establish
    // without racing the clock — a transient status (a source still being created, which is
    // what a caller replicating straight after a create meets) is the same guard on the same
    // predicate, and the sentence it answers is this constant, which reports no status at
    // all.
    let rows = [
        SourceStateCase {
            case: "a mysql source that has been stopped",
            source: "emu-src-state-stopped-mysql",
            replica: "emu-src-state-replica-1",
            engine: "mysql",
            by_arn: false,
        },
        SourceStateCase {
            case: "a postgres source that has been stopped, named by its ARN",
            source: "emu-src-state-stopped-pg",
            replica: "emu-src-state-replica-2",
            engine: "postgres",
            by_arn: true,
        },
        SourceStateCase {
            case: "a mariadb source that has been stopped",
            source: "emu-src-state-stopped-maria",
            replica: "emu-src-state-replica-3",
            engine: "mariadb",
            by_arn: false,
        },
    ];

    let mut sentences: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in &rows {
        let case = row.case;
        create_replica_source(&router, case, row.source, row.engine).await;
        poll_for_status(&router, case, row.source, "available").await;
        expect_ok(
            case,
            call(
                &router,
                "StopDBInstance",
                &[("DBInstanceIdentifier", row.source)],
            )
            .await,
        );
        poll_for_status(&router, case, row.source, "stopped").await;
        // The source really is in the status this row is about, read off the service's own
        // report rather than assumed.
        let reported = expect_ok(case, read_instance(&router, REGION, row.source).await);
        let status = text(&reported, "DBInstanceStatus");
        assert_eq!(
            status, "stopped",
            "case {case:?}: the row needs the source stopped, got {status:?}"
        );

        let named = if row.by_arn {
            resource_arn("db", row.source)
        } else {
            row.source.to_string()
        };
        let body = expect_error(
            case,
            create_read_replica(&router, REGION, row.replica, &named).await,
            400,
            INVALID_DB_INSTANCE_STATE,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, SOURCE_NOT_AVAILABLE,
            "case {case:?}: expected the fixed sentence {SOURCE_NOT_AVAILABLE:?}, got \
             {message:?}"
        );
        assert!(
            !message.contains(row.source) && !message.contains(status.as_str()),
            "case {case:?}: the refusal must report neither the source nor its status, got \
             {message:?}"
        );
        sentences.insert(message);
        // The refused create stored no replica.
        expect_error(
            case,
            read_instance(&router, REGION, row.replica).await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
    }
    assert_eq!(
        sentences.len(),
        1,
        "the guard answers ONE sentence for every source it refuses, got {sentences:?}"
    );

    // And the guard was about the STATE: the stopped source, started and running again,
    // serves the identical call.
    let restarted = "emu-src-state-stopped-mysql";
    expect_ok(
        "start the stopped source",
        call(
            &router,
            "StartDBInstance",
            &[("DBInstanceIdentifier", restarted)],
        )
        .await,
    );
    poll_for_status(&router, "the restarted source", restarted, "available").await;
    let served = expect_ok(
        "a replica of the restarted source",
        create_read_replica(&router, REGION, "emu-src-state-replica-ok", restarted).await,
    );
    assert_eq!(
        optional_text(&served, "ReadReplicaSourceDBInstanceIdentifier").as_deref(),
        Some(restarted),
        "case \"a replica of the restarted source\": got {served}"
    );
}
