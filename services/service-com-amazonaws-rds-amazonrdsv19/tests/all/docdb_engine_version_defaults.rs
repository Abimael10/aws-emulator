// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the engine version THIS front door provisions for a cluster of the
//! document engine whose create did not name an exact published release.
//!
//! The rule: where a request does not name an exact published engine version — it omits
//! `EngineVersion`, or names only a major line — the cluster runs the release the engine
//! catalogue DESIGNATES as the default (the engine's own default version, or the named
//! line's default), and that designated release is what every later report of the cluster
//! carries. The designation is published per release, not the newest release the catalogue
//! lists for the line.
//!
//! This door and the document engine's own control plane are two views of ONE engine
//! catalogue (`service_common::docdb_engine_catalog`), so the release they provision for
//! such a create must be the same one, whichever view the create arrived at: a client that
//! creates a cluster through one view and reads it back through the other must not see two
//! engine versions. The sibling suite in the DocumentDB crate
//! (`tests/all/engine_version_defaults.rs`) pins the same rule on the other view.
//!
//! Every row is read from the shared catalogue rather than written as a literal, so a row
//! added or re-designated there is covered here too.

use crate::db_clusters::the_cluster;
use crate::db_instances::{call, emulator, expect_ok, text};

/// The document engine, as its own control plane's `Engine` member publishes it.
const DOCUMENT_ENGINE: &str = ::service_common::docdb_engine_catalog::ENGINE;

/// Every published major line of that engine (`5.0`), oldest first and without repeats, and
/// the release the catalogue designates as one line's default: both read from the shared
/// catalogue's own accessors, so this suite asks the same question the handlers do.
fn published_lines() -> Vec<&'static str> {
    ::service_common::docdb_engine_catalog::major_lines()
}

/// The release the catalogue designates as the default of the line `line`.
fn designated_default_of_line(line: &str) -> &'static str {
    ::service_common::docdb_engine_catalog::line_default(line)
        .expect("every published line designates one of its releases its default")
        .version
}

/// A cluster of the document engine created through this door runs the designated default
/// when the create names no version, and the designated default of the LINE when it names
/// only a line — read back through the describe, since the stored record is what every later
/// call reports.
#[tokio::test]
async fn a_document_cluster_runs_the_designated_default_engine_version() {
    let router = emulator();

    // (case, what the create names, the release the cluster must run): the absent case, then
    // every published line, then one exact release the catalogue designates nothing about,
    // which a create must still provision exactly.
    let mut rows: Vec<(String, Option<&'static str>, &'static str)> = vec![(
        "a create naming no version".to_string(),
        None,
        ::service_common::docdb_engine_catalog::DEFAULT_ENGINE_VERSION,
    )];
    for line in published_lines() {
        rows.push((
            format!("a create naming the {line} line"),
            Some(line),
            designated_default_of_line(line),
        ));
    }
    for row in ::service_common::docdb_engine_catalog::engine_versions() {
        rows.push((
            format!("a create naming the release {} in full", row.version),
            Some(row.version),
            row.version,
        ));
    }

    for (index, (case, requested, expected)) in rows.iter().enumerate() {
        let identifier = format!("emu-docdb-default-{index}");
        let mut params: Vec<(&str, String)> = vec![
            ("DBClusterIdentifier", identifier.clone()),
            ("Engine", DOCUMENT_ENGINE.to_string()),
            ("MasterUsername", "emuadmin".to_string()),
            ("MasterUserPassword", "emu-Secret-99".to_string()),
        ];
        if let Some(requested) = requested {
            params.push(("EngineVersion", (*requested).to_string()));
        }
        let created = expect_ok(case, call(&router, "CreateDBCluster", &params).await);
        let described = expect_ok(
            case,
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        let stored = the_cluster(case, &described);
        assert_eq!(
            (
                text(&created, "EngineVersion"),
                text(&stored, "EngineVersion"),
            ),
            ((*expected).to_string(), (*expected).to_string()),
            "case {case:?}: expected the create and the read-back to report {expected:?}, got \
             {created} and {described}"
        );
    }
}
