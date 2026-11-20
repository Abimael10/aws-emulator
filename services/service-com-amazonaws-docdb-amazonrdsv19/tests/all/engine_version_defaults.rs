// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the engine version a cluster RUNS when its create did not name an
//! exact published release.
//!
//! The rule: where a request does not name an exact published engine version — it omits
//! `EngineVersion`, or names only a major line — the cluster runs the release the engine
//! catalogue DESIGNATES as the default (the engine's own default version, or the named
//! line's default), and that designated release is what every later report of the cluster
//! carries. The designation is published per release, so it is not the newest release the
//! catalogue lists for the line: a line publishes later patches than the one AWS designates,
//! and a create must not move a caller onto one of those.
//!
//! Every rule is driven over a TABLE spanning the class it generalises over — every
//! published line, every published release, and the absent case — through the same [`Docdb`]
//! harness the sibling suites use, so what is asserted is what a client reads off the wire.
//! The designated release is read from the shared catalogue
//! (`service_common::docdb_engine_catalog`), never written as a literal, so a catalogue row
//! added or re-designated there is covered here too.

use service_com_amazonaws_docdb_amazonrdsv19::state;

use crate::cluster_lifecycle::{count, text, texts, Docdb};

/// One published release of the shared catalogue.
type PublishedVersion = &'static ::service_common::docdb_engine_catalog::EngineVersion;

/// Every published release, oldest first.
fn published_versions() -> Vec<PublishedVersion> {
    ::service_common::docdb_engine_catalog::engine_versions().collect()
}

/// Every published major line (`5.0`), oldest first and without repeats, and the release
/// the catalogue designates as one line's default: both read from the shared catalogue's own
/// accessors, so this suite asks the same question the handlers do.
fn published_lines() -> Vec<&'static str> {
    ::service_common::docdb_engine_catalog::major_lines()
}

/// The release the catalogue designates as the default of the line `line`.
fn designated_default_of_line(line: &str) -> PublishedVersion {
    ::service_common::docdb_engine_catalog::line_default(line)
        .expect("every published line designates one of its releases its default")
}

/// Create a cluster, with `EngineVersion` carried only when the row names one, and hand back
/// the `EngineVersion` the create answered with and the one a read-back reports.
async fn created_and_stored_version(
    docdb: &Docdb,
    case: &str,
    identifier: &str,
    requested: Option<&str>,
) -> (String, String) {
    let mut params = vec![
        ("DBClusterIdentifier", identifier),
        ("Engine", state::DOCDB_ENGINE),
        ("MasterUsername", "admin"),
        ("MasterUserPassword", "secret99"),
    ];
    if let Some(requested) = requested {
        params.push(("EngineVersion", requested));
    }
    let created = docdb.ok(case, "CreateDBCluster", &params).await;
    let described = docdb
        .ok(
            case,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await;
    (
        text(&created, "EngineVersion"),
        text(&described, "EngineVersion"),
    )
}

/// A create that names no version at all runs the release the catalogue designates as the
/// engine's own default, and that release — not the newest patch its line publishes — is
/// what the stored cluster reports afterwards.
#[tokio::test]
async fn a_create_naming_no_version_runs_the_designated_engine_default() {
    let docdb = Docdb::new();

    let designated = state::DEFAULT_ENGINE_VERSION;
    let (created, stored) =
        created_and_stored_version(&docdb, "no version at all", "default-version", None).await;
    assert_eq!(
        (created.as_str(), stored.as_str()),
        (designated, designated),
        "case \"no version at all\": expected the create and the read-back to report the \
         designated default {designated:?}, got {created:?} and {stored:?}"
    );

    // The anti-maximum assertion, read from the catalogue rather than from a literal: the
    // designated default's own line publishes LATER releases, and the create picked none of
    // them. A default computed as the newest entry of the line would have.
    let designated_line = designated_default_of_line(
        ::service_common::docdb_engine_catalog::engine_version(designated)
            .expect("the engine's designated default is a published release")
            .major,
    );
    let later_in_its_line: Vec<&str> = published_versions()
        .into_iter()
        .filter(|row| {
            row.major == designated_line.major
                && ::service_common::rds_cluster_parameters::engine_version_precedes(
                    designated,
                    row.version,
                )
        })
        .map(|row| row.version)
        .collect();
    assert!(
        !later_in_its_line.is_empty(),
        "case \"no version at all\": the catalogue must publish a later release of the \
         designated line for this rule to have anything to distinguish, got \
         {later_in_its_line:?}"
    );
    assert!(
        !later_in_its_line.contains(&created.as_str()),
        "case \"no version at all\": expected the designated default rather than one of the \
         later releases {later_in_its_line:?} of its line, got {created:?}"
    );
}

/// A create naming only a LINE runs that line's designated default; a create naming an exact
/// published release runs exactly it, designated or not.
#[tokio::test]
async fn a_create_resolves_a_line_to_its_designated_default_and_a_release_to_itself() {
    let docdb = Docdb::new();

    // (case, what the request names, what the cluster must run) — every published line, then
    // every published release in full, so a release the catalogue designates nothing about
    // is covered in both shapes.
    let mut rows: Vec<(String, &'static str, &'static str)> = Vec::new();
    for line in published_lines() {
        rows.push((
            format!("the {line} line"),
            line,
            designated_default_of_line(line).version,
        ));
    }
    for row in published_versions() {
        rows.push((
            format!("the release {} in full", row.version),
            row.version,
            row.version,
        ));
    }

    for (index, (case, requested, expected)) in rows.iter().enumerate() {
        let identifier = format!("resolved-version-{index}");
        let (created, stored) =
            created_and_stored_version(&docdb, case, &identifier, Some(requested)).await;
        assert_eq!(
            (created.as_str(), stored.as_str()),
            (*expected, *expected),
            "case {case:?}: expected the create and the read-back to report {expected:?}, got \
             {created:?} and {stored:?}"
        );
    }
}

/// One row of the report table: the case, the request's parameters, and the release the
/// report must name.
type ReportRow = (String, Vec<(&'static str, &'static str)>, &'static str);

/// `DescribeDBEngineVersions` reports the SAME designation the creates above resolve
/// through: `DefaultOnly` names the engine's default version, and narrowed to one family it
/// names that line's designated default — so a client comparing its cluster's
/// `EngineVersion` against the report's default sees no difference.
#[tokio::test]
async fn the_engine_version_report_names_the_same_designated_defaults() {
    let docdb = Docdb::new();

    // (case, the request's parameters, the release the report must name)
    let mut rows: Vec<ReportRow> = vec![(
        "the engine's own default".to_string(),
        vec![("DefaultOnly", "true"), ("Engine", state::DOCDB_ENGINE)],
        state::DEFAULT_ENGINE_VERSION,
    )];
    for line in published_lines() {
        let designated = designated_default_of_line(line);
        rows.push((
            format!("the default of the {} family", designated.family),
            vec![
                ("DefaultOnly", "true"),
                ("DBParameterGroupFamily", designated.family),
            ],
            designated.version,
        ));
    }

    for (case, params, expected) in rows {
        let reported = docdb.ok(&case, "DescribeDBEngineVersions", &params).await;
        assert_eq!(
            (
                count(&reported, "DBEngineVersion"),
                texts(&reported, "EngineVersion").first().cloned(),
            ),
            (1, Some(expected.to_string())),
            "case {case:?}: expected exactly the designated default {expected:?}, got \
             {reported}"
        );
    }

    // And the engine's designated default is the release a create with no version runs, so
    // the report and the create cannot disagree: it is the value a client compares against.
    let (created, _) = created_and_stored_version(
        &docdb,
        "the create behind the report",
        "report-default-version",
        None,
    )
    .await;
    assert_eq!(
        created,
        state::DEFAULT_ENGINE_VERSION,
        "case \"the create behind the report\": expected the create to run the version the \
         report names as the engine's default, got {created:?}"
    );

    // A selection the catalogue designates nothing in has NO default to report: narrowed to
    // a release that is not a designated default, `DefaultOnly` answers with no version
    // rather than naming that release the default. Spanned over every undesignated release.
    for row in published_versions() {
        if row.default_of.is_line_default() {
            continue;
        }
        let case = format!("the default among only the undesignated {}", row.version);
        let reported = docdb
            .ok(
                &case,
                "DescribeDBEngineVersions",
                &[("DefaultOnly", "true"), ("EngineVersion", row.version)],
            )
            .await;
        assert_eq!(
            count(&reported, "DBEngineVersion"),
            0,
            "case {case:?}: expected no version, since the selection holds no designated \
             default, got {reported}"
        );
    }
}
