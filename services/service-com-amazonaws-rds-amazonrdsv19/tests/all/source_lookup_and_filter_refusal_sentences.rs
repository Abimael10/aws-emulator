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
//!   sentence a call whose own SUBJECT is that resource answers.
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

use crate::db_instances::{call, emulator, expect_error, text, REGION};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The Query API's own code for a member value the service will not serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// `DBParameterGroupNotFoundFault`'s `awsQueryError` code
/// (`smithy_query(query="shape", shape="DBParameterGroupNotFoundFault")`).
const DB_PARAMETER_GROUP_NOT_FOUND: &str = "DBParameterGroupNotFound";

/// `DBInstanceNotFoundFault`'s `awsQueryError` code.
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

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
