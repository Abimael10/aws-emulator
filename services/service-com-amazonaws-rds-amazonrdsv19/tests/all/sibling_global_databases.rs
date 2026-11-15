// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the global databases this one front door creates for EVERY control
//! plane it serves: `CreateGlobalCluster` -> `DescribeGlobalClusters` -> a duplicate
//! `CreateGlobalCluster` -> `DeleteGlobalCluster`.
//!
//! This model's wire identity (`endpointPrefix` / `sigv4Name` `rds`, `awsQuery`, version
//! `2014-10-31`) is published identically by the Amazon DocumentDB and Amazon Neptune
//! models, so a client compiled against either of those APIs signs for `rds` and is
//! answered HERE (`src/service.rs`). Each of the three control planes models a
//! `CreateGlobalCluster` of its own, and a global database of a sibling's engine is a
//! resource of THAT service.
//!
//! The rules pinned here, each over a TABLE rather than over one request:
//!
//! * a global database is created on every engine a control plane this door serves
//!   publishes for its own `CreateGlobalCluster`, reports the engine it was created on with
//!   the status a created global database has, and carries that one lifecycle through: the
//!   identifier-scoped report reads it back, a second create of the same identifier is the
//!   modelled `GlobalClusterAlreadyExistsFault`, the delete answers with the record it
//!   removed, and only then is the identifier not found;
//! * such a global database reports only the `GlobalCluster` members its OWN control plane
//!   publishes: the `Endpoint`, `EngineLifecycleSupport` and `StorageEncryptionType` of
//!   this model are members no sibling's `GlobalCluster` has.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each
//! assertion reads the XML this emulator itself answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    global_cluster_already_exists_message, DOCDB_ENGINE, GLOBAL_CLUSTER_ENGINES, NEPTUNE_ENGINE,
};

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};

/// Every engine a global database can be created on at this front door: the Aurora pair
/// this model's `CreateGlobalClusterMessage$Engine` publishes ("Valid Values: aurora-mysql
/// | aurora-postgresql") and the engine each sibling control plane publishes for its own
/// global database — `com.amazonaws.neptune#CreateGlobalClusterMessage$Engine` ("Valid
/// values: neptune") and `com.amazonaws.docdb#GlobalCluster$Engine` ("The Amazon
/// DocumentDB database engine used by the global cluster").
///
/// `pub(crate)` because it is the published set this crate's refusal table is the
/// complement of, so neither can drift from the other.
pub(crate) const GLOBAL_DATABASE_ENGINES: [&str; 4] = [
    GLOBAL_CLUSTER_ENGINES[0],
    GLOBAL_CLUSTER_ENGINES[1],
    DOCDB_ENGINE,
    NEPTUNE_ENGINE,
];

/// The status a created global database reports: `GlobalCluster$Status` "specifies the
/// current state of this global database", and a global database that provisions no DB
/// cluster of its own is usable the moment it exists.
const STATUS_AVAILABLE: &str = "available";

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A global cluster's ARN, which carries NO region: it is an account-scoped resource.
fn global_cluster_arn(identifier: &str) -> String {
    format!("arn:aws:rds::{ACCOUNT}:global-cluster:{identifier}")
}

/// One row of the lifecycle table: an engine a control plane here publishes, and the two
/// booleans the create asks for.
struct Row {
    case: &'static str,
    engine: &'static str,
    /// `StorageEncrypted`, or `None` for a create that names none.
    storage_encrypted: Option<bool>,
    /// `DeletionProtection`, or `None` for a create that names none.
    deletion_protection: Option<bool>,
}

/// The rows: every engine of [`GLOBAL_DATABASE_ENGINES`], differing along what the rule
/// generalises over beyond the engine — whether the create names each of the two settings
/// `CreateGlobalCluster` publishes for a global database and which value it asks for, so
/// the engine is the only thing that could decide the verdict.
const ROWS: &[Row] = &[
    Row {
        case: "aurora-mysql, both settings named",
        engine: GLOBAL_DATABASE_ENGINES[0],
        storage_encrypted: Some(true),
        deletion_protection: Some(false),
    },
    Row {
        case: "aurora-postgresql, neither setting named",
        engine: GLOBAL_DATABASE_ENGINES[1],
        storage_encrypted: None,
        deletion_protection: None,
    },
    Row {
        case: "the Amazon DocumentDB engine, unencrypted and unprotected",
        engine: GLOBAL_DATABASE_ENGINES[2],
        storage_encrypted: Some(false),
        deletion_protection: Some(false),
    },
    Row {
        case: "the Amazon Neptune engine, encrypted and unprotected",
        engine: GLOBAL_DATABASE_ENGINES[3],
        storage_encrypted: Some(true),
        deletion_protection: Some(false),
    },
    Row {
        case: "the Amazon Neptune engine, neither setting named",
        engine: GLOBAL_DATABASE_ENGINES[3],
        storage_encrypted: None,
        deletion_protection: None,
    },
];

/// The create's parameters for one row.
fn create_params(row: &Row, identifier: &str) -> Vec<(&'static str, String)> {
    let mut params: Vec<(&'static str, String)> = vec![
        ("GlobalClusterIdentifier", identifier.to_string()),
        ("Engine", row.engine.to_string()),
    ];
    if let Some(encrypted) = row.storage_encrypted {
        params.push(("StorageEncrypted", encrypted.to_string()));
    }
    if let Some(protected) = row.deletion_protection {
        params.push(("DeletionProtection", protected.to_string()));
    }
    params
}

#[tokio::test]
async fn a_global_database_is_created_on_every_engine_a_control_plane_here_publishes() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let identifier = format!("emu-sgd-{index}");

        // 1. The create answers with the global database it minted.
        let created = expect_ok(
            case,
            call(&router, "CreateGlobalCluster", &create_params(row, &identifier)).await,
        );
        for (member, expected) in [
            ("GlobalClusterIdentifier", identifier.clone()),
            ("Engine", row.engine.to_string()),
            ("Status", STATUS_AVAILABLE.to_string()),
            ("GlobalClusterArn", global_cluster_arn(&identifier)),
            (
                "StorageEncrypted",
                row.storage_encrypted.unwrap_or(false).to_string(),
            ),
            (
                "DeletionProtection",
                row.deletion_protection.unwrap_or(false).to_string(),
            ),
        ] {
            let actual = text(&created, member);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {member} {expected:?}, got {actual:?} in {created}"
            );
        }
        // The version the global database runs is the engine's own, resolved by the
        // service rather than echoed: the request named none, so it must still report one
        // and it must be a version of the engine's catalogue.
        let version = text(&created, "EngineVersion");
        assert!(
            !version.is_empty(),
            "case {case:?}: expected the engine version the global database runs, got \
             {created}"
        );
        // An immutable server-minted id, and no member yet: "You can create a global
        // database that is initially empty."
        assert!(
            !text(&created, "GlobalClusterResourceId").is_empty(),
            "case {case:?}: expected a minted GlobalClusterResourceId, got {created}"
        );
        assert!(
            subtrees(&created, "GlobalClusterMember").is_empty(),
            "case {case:?}: a global database created empty must report no member, got \
             {created}"
        );
        assert!(
            subtrees(&created, "Tag").is_empty(),
            "case {case:?}: a global database created with no tag must report none, got \
             {created}"
        );

        // 2. The identifier-scoped report reads the record back, member for member.
        let reported = expect_ok(
            case,
            call(
                &router,
                "DescribeGlobalClusters",
                &[("GlobalClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        for member in [
            "GlobalClusterIdentifier",
            "GlobalClusterResourceId",
            "GlobalClusterArn",
            "Status",
            "Engine",
            "EngineVersion",
            "StorageEncrypted",
            "DeletionProtection",
        ] {
            let (from_create, from_report) = (text(&created, member), text(&reported, member));
            assert_eq!(
                from_report, from_create,
                "case {case:?}: expected the report to carry the created {member} \
                 {from_create:?}, got {from_report:?} in {reported}"
            );
        }

        // 3. A second create of the same identifier is the modelled already-exists fault,
        //    whatever engine it names — the conflict is the identifier's, so it is checked
        //    for every engine this door takes.
        let duplicate = expect_error(
            case,
            call(&router, "CreateGlobalCluster", &create_params(row, &identifier)).await,
            400,
            "GlobalClusterAlreadyExistsFault",
        );
        let message = text(&duplicate, "Message");
        let expected = global_cluster_already_exists_message(&identifier);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?} in {duplicate}"
        );

        // 4. The delete answers with the global database it removed, and only then is the
        //    identifier not found.
        let deleted = expect_ok(
            case,
            call(
                &router,
                "DeleteGlobalCluster",
                &[("GlobalClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        for member in ["GlobalClusterIdentifier", "GlobalClusterArn", "Engine"] {
            let (from_create, from_delete) = (text(&created, member), text(&deleted, member));
            assert_eq!(
                from_delete, from_create,
                "case {case:?}: expected the delete to answer with the removed {member} \
                 {from_create:?}, got {from_delete:?} in {deleted}"
            );
        }
        expect_error(
            case,
            call(
                &router,
                "DescribeGlobalClusters",
                &[("GlobalClusterIdentifier", identifier.as_str())],
            )
            .await,
            404,
            "GlobalClusterNotFoundFault",
        );
    }
}

/// The members of this model's `GlobalCluster` that no sibling control plane's
/// `GlobalCluster` publishes: neither `com.amazonaws.neptune#GlobalCluster` nor
/// `com.amazonaws.docdb#GlobalCluster` carries any of them
/// (`smithy_query(query="shape",shape="GlobalCluster")` on either model).
const MEMBERS_OF_THIS_MODEL_ALONE: [&str; 3] =
    ["Endpoint", "EngineLifecycleSupport", "StorageEncryptionType"];

#[tokio::test]
async fn a_global_database_reports_only_the_members_its_own_control_plane_publishes() {
    let router = emulator();

    for (index, engine) in GLOBAL_DATABASE_ENGINES.iter().enumerate() {
        let identifier = format!("emu-sgd-members-{index}");
        let created = expect_ok(
            engine,
            call(
                &router,
                "CreateGlobalCluster",
                &[
                    ("GlobalClusterIdentifier", identifier.as_str()),
                    ("Engine", engine),
                ],
            )
            .await,
        );
        let reported = expect_ok(
            engine,
            call(
                &router,
                "DescribeGlobalClusters",
                &[("GlobalClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        // A sibling control plane's global database is a resource of THAT service, so the
        // three members only this model publishes are absent from it — and present for
        // this model's own Aurora global databases, so their absence is the engine's doing
        // and not a projection that dropped them for everyone.
        let a_siblings_engine = *engine == DOCDB_ENGINE || *engine == NEPTUNE_ENGINE;
        for member in MEMBERS_OF_THIS_MODEL_ALONE {
            for (source, body) in [("the create", &created), ("the report", &reported)] {
                let answered = optional_text(body, member);
                assert_eq!(
                    answered.is_none(),
                    a_siblings_engine,
                    "case {engine:?}: {source} must {} {member}, got {answered:?} in {body}",
                    if a_siblings_engine { "omit" } else { "carry" }
                );
            }
        }
    }
}
