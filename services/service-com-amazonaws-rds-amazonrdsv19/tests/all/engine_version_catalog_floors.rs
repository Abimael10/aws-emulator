// SPDX-License-Identifier: CC0-1.0

//! The BUILDS the engine-version catalogue reaches for each major-version family it publishes.
//!
//! THE RULE: `DescribeDBEngineVersions` serves the catalogue AWS serves, so for every
//! major-version family of an engine the newest minor the report lists is at or past the build
//! AWS ships for that family — and that newest build is what the family resolves to, which is
//! why a create naming only the major version is provisioned on it. A family whose rows stop at
//! an older build tells a client that asks for a current minor that the version is not
//! available, and makes version-compatibility logic driven off the catalogue pick a build AWS no
//! longer offers as its newest.
//!
//! Every listed version also NAMES its own family: `MajorEngineVersion` is the major-version line
//! the version belongs to, and `DBParameterGroupFamily` is that line's parameter-group family, so
//! a client can group the report by family without parsing the version string.
//!
//! The floors below are the builds AWS has long shipped per family (see [`FLOORS`] for the
//! source), spanning both PostgreSQL-compatible engines and every family of each that the report
//! lists as available — including families the issue this suite pins did not cite.

use crate::db_engine_versions::{all_versions_of, Version};
use crate::db_instances::{call, emulator, expect_ok, text};
use smithy_server_runtime::MultiServiceRouter;

/// One floor: the engine, the major-version family, and the minor the family's newest listed
/// build must be at or past.
///
/// SOURCE: the release lists AWS publishes per family for RDS for PostgreSQL and Aurora
/// PostgreSQL — whose minors are the community PostgreSQL releases of that line — read at the
/// builds that have been offered for well over a year. The floor is what the rule requires; the
/// exact newest build of any one family is not.
type Floor = (&'static str, &'static str, u64);

const FLOORS: &[Floor] = &[
    // The two legs the catalogue was found behind on.
    ("postgres", "16", 8),
    ("aurora-postgresql", "15", 10),
    // And every other family of those two engines the report lists as available, so the rule is
    // asserted where it holds rather than only where it was found broken.
    ("postgres", "13", 20),
    ("postgres", "14", 17),
    ("postgres", "15", 12),
    ("postgres", "17", 4),
    ("aurora-postgresql", "13", 15),
    ("aurora-postgresql", "14", 12),
    ("aurora-postgresql", "16", 8),
    ("aurora-postgresql", "17", 4),
];

/// The MINOR of an engine version in the family `family`: the number right after the family's own
/// prefix, or `None` for a version of another family or one whose minor is not a number (an RDS
/// Extended Support build, `11.22-rds.20241121`, whose minor is the community release it
/// continues).
fn minor_in_family(version: &str, family: &str) -> Option<u64> {
    let rest = version.strip_prefix(family)?.strip_prefix('.')?;
    rest.split(['.', '-']).next()?.parse().ok()
}

/// The versions of `engine` the report lists as AVAILABLE — the ones a create may name.
async fn offered_versions(router: &MultiServiceRouter, engine: &str) -> Vec<Version> {
    all_versions_of(router, engine)
        .await
        .into_iter()
        .filter(|version| version.status == "available")
        .collect()
}

#[tokio::test]
async fn every_familys_newest_listed_minor_is_at_or_past_the_build_aws_ships() {
    let router = emulator();
    for (engine, family, floor) in FLOORS {
        let minors: Vec<u64> = offered_versions(&router, engine)
            .await
            .iter()
            .filter_map(|version| minor_in_family(&version.engine_version, family))
            .collect();
        let newest = minors.iter().copied().max();
        assert!(
            newest.is_some_and(|newest| newest >= *floor),
            "case {engine} {family}.x: the newest listed minor must be at or past \
             {family}.{floor}, got the minors {minors:?}"
        );
    }
}

#[tokio::test]
async fn every_listed_version_names_the_major_family_it_belongs_to() {
    let router = emulator();
    for (engine, family, _floor) in FLOORS {
        let versions = offered_versions(&router, engine).await;
        let of_family: Vec<&Version> = versions
            .iter()
            .filter(|version| minor_in_family(&version.engine_version, family).is_some())
            .collect();
        assert!(
            !of_family.is_empty(),
            "case {engine} {family}.x: the report must list the family at all"
        );
        for version in of_family {
            assert_eq!(
                version.major_engine_version, *family,
                "case {engine} {}: expected MajorEngineVersion {family}, got {:?}",
                version.engine_version, version.major_engine_version
            );
            // The family's own parameter-group family, which is the line's family and may carry
            // a suffix for a build that has a parameter-group family of its own inside the line
            // (Aurora Limitless Database, `aurora-postgresql16-limitless`).
            assert!(
                version
                    .parameter_group_family
                    .starts_with(&format!("{engine}{family}")),
                "case {engine} {}: expected a parameter group family of the line \
                 {engine}{family}, got {:?}",
                version.engine_version,
                version.parameter_group_family
            );
        }
    }
}

#[tokio::test]
async fn a_create_naming_only_the_major_version_is_provisioned_on_the_familys_newest_build() {
    let router = emulator();
    // The two engines' own creates: a DB instance for the instance engine and a DB cluster for
    // the cluster engine, each naming the major version alone.
    for (engine, family, _floor) in FLOORS {
        let newest = offered_versions(&router, engine)
            .await
            .iter()
            .filter_map(|version| {
                minor_in_family(&version.engine_version, family)
                    .map(|minor| (minor, version.engine_version.clone()))
            })
            .max_by_key(|(minor, _)| *minor)
            .map(|(_, version)| version)
            .unwrap_or_else(|| panic!("case {engine} {family}.x: the family must be listed"));
        let case = format!("{engine} {family} resolves to {newest}");
        let identifier = format!("emu-floor-{engine}-{family}");
        let reported = match engine == &"postgres" {
            true => {
                let body = expect_ok(
                    &case,
                    call(
                        &router,
                        "CreateDBInstance",
                        &[
                            ("DBInstanceIdentifier", identifier.as_str()),
                            ("Engine", engine),
                            ("DBInstanceClass", "db.t4g.micro"),
                            ("EngineVersion", family),
                            ("MasterUsername", "emuadmin"),
                            ("MasterUserPassword", "a-strong-password"),
                        ],
                    )
                    .await,
                );
                text(&body, "EngineVersion")
            }
            false => {
                let body = expect_ok(
                    &case,
                    call(
                        &router,
                        "CreateDBCluster",
                        &[
                            ("DBClusterIdentifier", identifier.as_str()),
                            ("Engine", engine),
                            ("EngineVersion", family),
                            ("MasterUsername", "emuadmin"),
                            ("MasterUserPassword", "a-strong-password"),
                        ],
                    )
                    .await,
                );
                text(&body, "EngineVersion")
            }
        };
        assert_eq!(
            reported, newest,
            "case {case:?}: expected the family's newest listed build, got {reported:?}"
        );
    }
}
