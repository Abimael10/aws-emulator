// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how this front door reads a member that NAMES an existing DB
//! cluster, and for the code it refuses a request its own model's rules rejected under.
//!
//! The rules under test, each one level above the call that exposed it:
//!
//! * A member that names a DB cluster takes "the user-supplied DB cluster identifier or the
//!   Amazon Resource Name (ARN) of the DB cluster", so a reference whose identifier is
//!   outside the grammar every DB cluster identifier is assigned under cannot name a DB
//!   cluster at all: it is refused as a parameter value the member does not accept
//!   (`InvalidParameterValue`, HTTP 400) BEFORE any record is read, and the modelled
//!   `DBClusterNotFoundFault` is left for a WELL-FORMED identifier that names nothing.
//! * The fault that reports such an identifier names the IDENTIFIER the reference carries,
//!   not the ARN as supplied: the ARN form is reduced to the identifier it names before the
//!   lookup, so one cluster is reported by one name whichever form the request used.
//! * A request the model's own rules refused — a value outside a declared constraint, and a
//!   `@required` member the request did not carry — is answered with the Query API code this
//!   front door publishes for a parameter it was not given acceptably, and a parameter the
//!   request never carried is refused by naming the parameter it has to supply (the sentence
//!   the sibling table in `missing_parameter_refusal_sentences.rs` holds this door to).
//!
//! Each table spans what its rule generalises over: which operation the reference reached and
//! which clause of the grammar the value broke; the forms a reference can take (identifier,
//! ARN, each in more than one case); and which member of which operation the request left
//! out. The recorded request is ONE row of each.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with.

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts, REGION};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The Query API's own code for a parameter this front door was not given acceptably.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// `DBClusterNotFoundFault`'s `awsQueryError` code
/// (`smithy_query(query="shape", shape="DBClusterNotFoundFault")`).
const DB_CLUSTER_NOT_FOUND: &str = "DBClusterNotFoundFault";

/// The engine the clusters below are created with.
const ENGINE: &str = "aurora-mysql";

/// The sentence a value no DB cluster identifier could be is refused with: the subject this
/// member's refusals are worded by, and the value exactly as the request sent it. The two
/// spaces after the colon are part of the template.
fn invalid_identifier_message(value: &str) -> String {
    format!("Invalid database cluster identifier:  {value}")
}

/// The sentence the lookup fault carries for the cluster identifier `identifier`.
fn not_found_message(identifier: &str) -> String {
    format!("DBCluster {identifier} not found.")
}

/// The ARN of the DB cluster `identifier` in the region the harness signs for.
fn cluster_arn(identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{identifier}")
}

/// The cluster identifier a reference NAMES: the reference itself, or the resource field of
/// an ARN — the reduction the API's two published forms of the member share, written out here
/// so the table pins it rather than reading it from the code under test.
fn identifier_named(reference: &str) -> &str {
    match reference.strip_prefix("arn:") {
        None => reference,
        Some(rest) => rest.rsplit(':').next().unwrap_or(reference),
    }
}

/// One operation that resolves the DB cluster its `DBClusterIdentifier` names, and the
/// members it carries besides that one.
struct ClusterCall {
    case: &'static str,
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
}

/// The operations whose `DBClusterIdentifier` names the cluster the call addresses: the
/// report, the delete, the modification, the three lifecycle calls, the failover, the
/// promotion, the snapshot and the two IAM-role associations. `extra` carries each one's
/// other required members, all of them well-formed, so the only thing a row varies is the
/// cluster reference.
const CLUSTER_CALLS: &[ClusterCall] = &[
    ClusterCall {
        case: "the report of one cluster",
        action: "DescribeDBClusters",
        extra: &[],
    },
    ClusterCall {
        case: "the report of a cluster's automated backups",
        action: "DescribeDBClusterAutomatedBackups",
        extra: &[],
    },
    ClusterCall {
        case: "the delete",
        action: "DeleteDBCluster",
        extra: &[("SkipFinalSnapshot", "true")],
    },
    ClusterCall {
        case: "the modification",
        action: "ModifyDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the start",
        action: "StartDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the stop",
        action: "StopDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the reboot",
        action: "RebootDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the failover",
        action: "FailoverDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the read-replica promotion",
        action: "PromoteReadReplicaDBCluster",
        extra: &[],
    },
    ClusterCall {
        case: "the snapshot of a cluster",
        action: "CreateDBClusterSnapshot",
        extra: &[("DBClusterSnapshotIdentifier", "emu-lookup-snapshot")],
    },
    ClusterCall {
        case: "the IAM-role association",
        action: "AddRoleToDBCluster",
        extra: &[("RoleArn", "arn:aws:iam::000000000000:role/emu-lookup-role")],
    },
    ClusterCall {
        case: "the IAM-role disassociation",
        action: "RemoveRoleFromDBCluster",
        extra: &[("RoleArn", "arn:aws:iam::000000000000:role/emu-lookup-role")],
    },
];

/// The parameters of one call: the cluster reference plus the row's own members.
fn request(row: &ClusterCall, named: &str) -> Vec<(String, String)> {
    let mut params = vec![("DBClusterIdentifier".to_string(), named.to_string())];
    params.extend(
        row.extra
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    params
}

#[tokio::test]
async fn a_reference_no_cluster_identifier_could_be_is_refused_as_a_parameter_value() {
    let router = emulator();
    // Each clause of the grammar the member publishes: "Must contain from 1 to 63 letters,
    // numbers, or hyphens. First character must be a letter. Can't end with a hyphen or
    // contain two consecutive hyphens."
    let malformed = [
        ("a character the alphabet excludes", "Bad_Name!".to_string()),
        ("a space inside the value", "emu lookup".to_string()),
        ("a leading digit", "1-emu-lookup".to_string()),
        ("a leading hyphen", "-emu-lookup".to_string()),
        ("a trailing hyphen", "emu-lookup-".to_string()),
        ("two consecutive hyphens", "emu--lookup".to_string()),
        ("one character past the published bound", "e".repeat(64)),
        (
            "an ARN whose identifier breaks the grammar",
            cluster_arn("emu--lookup"),
        ),
    ];

    for row in CLUSTER_CALLS {
        for (shape, value) in &malformed {
            let case = format!("{}, {shape}", row.case);
            let body = expect_error(
                &case,
                call(&router, row.action, &request(row, value)).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&body, "Message");
            // An ARN-form reference is reduced to the identifier it names, so that is the
            // value the refusal quotes.
            let quoted = identifier_named(value);
            let expected = invalid_identifier_message(quoted);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_well_formed_identifier_that_names_nothing_is_the_modelled_not_found() {
    let router = emulator();
    // Values the grammar admits, so each one COULD name a cluster and none of them does:
    // the answer is the fault the operation models, not a refused parameter. The last two
    // carry no identifier at all, which names no cluster either — and is what an ARN whose
    // resource field is empty reduces to.
    let admitted = [
        ("a plain identifier", "emu-lookup-absent".to_string()),
        ("a single letter", "c".to_string()),
        ("digits and hyphens", "emu-lookup-8-0".to_string()),
        ("mixed case, which the member is not sensitive to", "EMU-Lookup-Absent".to_string()),
        ("the longest identifier the member publishes", "e".repeat(63)),
        ("the empty value", String::new()),
        ("an ARN whose resource field is empty", cluster_arn("")),
    ];

    for (shape, value) in &admitted {
        let case = format!("the report of one cluster, {shape}");
        let body = expect_error(
            &case,
            call(&router, "DescribeDBClusters", &[("DBClusterIdentifier", value)]).await,
            404,
            DB_CLUSTER_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = not_found_message(identifier_named(value));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_cluster_named_by_its_arn_is_reported_by_its_identifier() {
    let router = emulator();
    // The fault names the IDENTIFIER the reference carries: an ARN is reduced to it before
    // the lookup, so the two published forms of the member report one and the same name.
    let rows = [
        (
            "an ARN of a cluster that does not exist",
            cluster_arn("emu-arn-absent"),
            "emu-arn-absent",
        ),
        (
            "an ARN whose identifier carries digits and hyphens",
            cluster_arn("emu-arn-absent-8-0"),
            "emu-arn-absent-8-0",
        ),
        (
            "an ARN in another partition",
            format!("arn:aws-us-gov:rds:{REGION}:{ACCOUNT}:cluster:emu-arn-gov"),
            "emu-arn-gov",
        ),
        (
            "an ARN of another region's cluster",
            format!("arn:aws:rds:eu-west-1:{ACCOUNT}:cluster:emu-arn-elsewhere"),
            "emu-arn-elsewhere",
        ),
        (
            "the identifier form of the same member",
            "emu-arn-plain".to_string(),
            "emu-arn-plain",
        ),
    ];

    for (case, named, identifier) in &rows {
        let body = expect_error(
            case,
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", named)],
            )
            .await,
            404,
            DB_CLUSTER_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = not_found_message(identifier);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains("arn:"),
            "case {case:?}: the fault names the identifier, not the ARN, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_stored_cluster_is_still_reported_through_either_form_of_the_member() {
    let router = emulator();
    let identifier = "emu-lookup-roundtrip";
    expect_ok(
        "create the cluster",
        call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", ENGINE),
            ],
        )
        .await,
    );

    // The gate refuses only references no cluster identifier could be: every form the
    // member publishes still resolves the cluster the create stored.
    let rows = [
        ("the identifier as created", identifier.to_string()),
        ("the identifier in another case", identifier.to_ascii_uppercase()),
        ("the cluster's ARN", cluster_arn(identifier)),
    ];

    for (case, named) in &rows {
        let body = expect_ok(
            case,
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", named)],
            )
            .await,
        );
        let reported = texts(&body, "DBClusterIdentifier");
        assert_eq!(
            reported,
            vec![identifier.to_string()],
            "case {case:?}: expected the stored cluster reported once, got {reported:?} in {body}"
        );
    }
}

/// One request that leaves a `@required` member out: the operation, the members it does
/// carry, and the member path the validation sentence names.
struct AbsentMemberCase {
    case: &'static str,
    action: &'static str,
    params: &'static [(&'static str, &'static str)],
    /// The parameter the refusal names — the model's member name with its first character
    /// lowered, which is the casing a refusal of this front door spells a member in.
    member_path: &'static str,
}

#[tokio::test]
async fn an_absent_required_member_is_refused_as_a_parameter_value() {
    let router = emulator();
    // One row per operation and member, so the rule is read off the refusal seam rather
    // than off one call: each request carries every required member but one.
    let rows = [
        AbsentMemberCase {
            case: "a cluster create with no Engine",
            action: "CreateDBCluster",
            params: &[("DBClusterIdentifier", "emu-absent-engine")],
            member_path: "engine",
        },
        AbsentMemberCase {
            case: "a cluster create with no DBClusterIdentifier",
            action: "CreateDBCluster",
            params: &[("Engine", ENGINE)],
            member_path: "dBClusterIdentifier",
        },
        AbsentMemberCase {
            case: "a cluster delete with no DBClusterIdentifier",
            action: "DeleteDBCluster",
            params: &[("SkipFinalSnapshot", "true")],
            member_path: "dBClusterIdentifier",
        },
        AbsentMemberCase {
            case: "a cluster parameter-group modification with no Parameters",
            action: "ModifyDBClusterParameterGroup",
            params: &[("DBClusterParameterGroupName", "emu-absent-parameters")],
            member_path: "parameters",
        },
        AbsentMemberCase {
            case: "an instance create with no DBInstanceClass",
            action: "CreateDBInstance",
            params: &[
                ("DBInstanceIdentifier", "emu-absent-class"),
                ("Engine", "mysql"),
            ],
            member_path: "dBInstanceClass",
        },
        AbsentMemberCase {
            case: "a tag read with no ResourceName",
            action: "ListTagsForResource",
            params: &[],
            member_path: "resourceName",
        },
    ];

    for row in &rows {
        let body = expect_error(
            row.case,
            call(&router, row.action, row.params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!(
            "Please specify a value for parameter '{}'.",
            row.member_path
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }
}
