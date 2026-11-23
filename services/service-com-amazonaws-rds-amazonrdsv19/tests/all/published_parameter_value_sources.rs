// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHERE the value a parameter report carries comes from, and for the
//! domain such a value is held to.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on, so every assertion reads the XML the emulator itself produced.
//!
//! The rule under test: `Parameter$Source` is "the source of the parameter value", so a
//! published parameter is reported under the source the value it carries comes FROM — the
//! managed service for an entry whose value that service supplies (the engine installation it
//! owns, or a value its own default parameter group pins), the engine's own build for every
//! other entry, and the caller for a value a group has set. The same answer is what a report's
//! `Source` SELECTS by, and an entry the service supplies the value of carries that value
//! rather than reporting none.
//!
//! The second rule under test: `AllowedValues` is the domain the engine publishes, whatever
//! its magnitude — a cache size in bytes on a 64-bit build is bounded by the unsigned 64-bit
//! range, past the largest signed 64-bit integer — and a request is held to that domain: the
//! published bound is admitted and one step past it is refused.
//!
//! The tables below are LITERALS: the service-supplied entries and their values, and the
//! published domain, are the ones these engines' own published defaults carry, so a drift in
//! this service's catalog fails this test instead of moving with it. They span the families
//! that share the MySQL vocabulary (two lines of the engine itself, Aurora MySQL and
//! MariaDB), an engine vocabulary that supplies no value of its own, both ends of the
//! published range and one step past each, and entries no issue cited.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    APPLY_METHOD_PENDING_REBOOT, PARAMETER_SOURCE_ENGINE_DEFAULT, PARAMETER_SOURCE_SYSTEM,
    PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};
use crate::engine_default_parameters::{reported, Reported};

/// The engine-default report under test.
const ACTION: &str = "DescribeEngineDefaultParameters";

/// The error code Amazon RDS answers a parameter value outside a published domain with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The families that share the MySQL parameter vocabulary: two release lines of the engine
/// itself, the Aurora MySQL engine that runs the same vocabulary, and the MariaDB engine that
/// runs it under its own release numbering. One vocabulary, so each of them publishes the same
/// value sources.
const MYSQL_FAMILIES: [&str; 4] = ["mysql5.7", "mysql8.0", "aurora-mysql8.0", "mariadb10.11"];

/// A family whose engine supplies no value of its own, which is the complement of the rule:
/// every entry it publishes is the engine's own default.
const ENGINE_ONLY_FAMILY: &str = "postgres16";

/// One row of the source table: the case, the parameter, the `Source` it must be reported
/// under, and the `ParameterValue` it must carry (`None` where the engine publishes none).
type Sourced = (&'static str, &'static str, &'static str, Option<&'static str>);

/// What each named parameter of the MySQL vocabulary reports, read from these engines' own
/// published defaults: the installation directory the managed service owns and the binary-log
/// cache size its own defaults pin come from that service and carry the value it supplies,
/// while an entry the engine's build settles comes from the engine. The last three rows are
/// entries no issue cited, spanning both apply types and three data types.
const SOURCED: [Sourced; 5] = [
    (
        "the installation directory the managed service owns",
        "basedir",
        PARAMETER_SOURCE_SYSTEM,
        Some("/rdsdbbin/mysql"),
    ),
    (
        "a value the managed service's own defaults pin",
        "binlog_cache_size",
        PARAMETER_SOURCE_SYSTEM,
        Some("32768"),
    ),
    (
        "an entry the engine's own build settles",
        "autocommit",
        PARAMETER_SOURCE_ENGINE_DEFAULT,
        Some("1"),
    ),
    (
        "another engine-settled entry, of a different data type",
        "binlog_checksum",
        PARAMETER_SOURCE_ENGINE_DEFAULT,
        Some("CRC32"),
    ),
    (
        // Autosized by the engine at startup, so no value is published for it at all — and
        // an entry with no value is still the engine's.
        "an entry the engine autosizes",
        "back_log",
        PARAMETER_SOURCE_ENGINE_DEFAULT,
        None,
    ),
];

/// The published domain of the parameter the range rows below drive, as the MySQL-compatible
/// engines publish it: from the smallest binary-log cache the server admits to the largest its
/// 64-bit build does. Its upper bound is past the largest SIGNED 64-bit integer, which is the
/// case this table exists for.
const RANGED_PARAMETER: &str = "binlog_cache_size";

/// That parameter's published `AllowedValues`, written as the API reports it.
const RANGED_DOMAIN: &str = "4096-18446744073709547520";

/// One row of the domain table: the case, the value, and whether the published domain admits
/// it — each published bound, a value inside, one step past each bound, and a value that is no
/// number at all.
const DOMAIN: [(&str, &str, bool); 6] = [
    ("the published low bound", "4096", true),
    ("a value inside the domain", "1048576", true),
    ("the published high bound", "18446744073709547520", true),
    ("one step below the low bound", "4095", false),
    ("one step past the high bound", "18446744073709547521", false),
    ("a value that is no number", "not-a-number", false),
];

/// Every `<Parameter>` the engine-default report of `family` carries, paginated to the end.
async fn published_defaults(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
) -> Vec<Reported> {
    let mut found: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    // The longest catalog this service publishes is well inside eight pages of 100; the loop
    // is bounded so a marker that never clears cannot hang the test.
    for _ in 0..8 {
        let mut params: Vec<(&str, String)> =
            vec![("DBParameterGroupFamily", family.to_string())];
        if let Some(marker) = &marker {
            params.push(("Marker", marker.clone()));
        }
        let body = expect_ok(case, call(router, ACTION, &params).await);
        found.extend(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return found;
        }
    }
    panic!("case {case:?}: paging did not terminate after {} rows", found.len());
}

/// The rows a group reports under `source`, or every row at once where the request names none.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    source: Option<&str>,
) -> Vec<Reported> {
    let extra: Vec<(&str, String)> = source
        .map(|source| vec![("Source", source.to_string())])
        .unwrap_or_default();
    let mut params = extra;
    params.push(("MaxRecords", "100".to_string()));
    let body = expect_ok(case, describe(router, group, &params).await);
    reported(&body)
}

/// The row named `name`, or a panic naming what was reported instead.
fn row<'a>(case: &str, rows: &'a [Reported], name: &str) -> &'a Reported {
    rows.iter().find(|row| row.name == name).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected {name} among the reported parameters, got {:?}",
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>()
        )
    })
}

/// Each published parameter is reported under the source the value it carries comes from, at
/// the family level and in the listing of a group of that family alike.
#[tokio::test]
async fn a_published_parameter_is_reported_under_the_source_of_its_value() {
    let router = emulator();

    for (index, family) in MYSQL_FAMILIES.into_iter().enumerate() {
        let group_case = format!("family {family}");
        let group = create_group(
            &router,
            &group_case,
            &format!("emu-value-source-{index}"),
            family,
        )
        .await;
        let defaults = published_defaults(&router, &group_case, family).await;
        let group_rows = listed(&router, &group_case, &group, None).await;

        for (case, name, source, value) in SOURCED {
            let case = format!("{family}: {case} ({name})");
            for (level, rows) in [("the family's defaults", &defaults), ("a group's listing", &group_rows)] {
                let found = row(&case, rows, name);
                assert_eq!(
                    (found.source.as_deref(), found.value.as_deref()),
                    (Some(source), value),
                    "case {case:?}: {level} must report the value's own source and that \
                     value, got {found:?}"
                );
            }
        }

        // The sources partition the published list: every row of the group's listing is
        // reported under one of the two published sources — nobody has set anything, so none
        // is the caller's — and the row's apply method is the group's own either way.
        for found in &group_rows {
            assert!(
                matches!(
                    found.source.as_deref(),
                    Some(PARAMETER_SOURCE_SYSTEM) | Some(PARAMETER_SOURCE_ENGINE_DEFAULT)
                ),
                "case {group_case:?}: {} is reported under a published source, got {found:?}",
                found.name
            );
            assert_eq!(
                found.apply_method.as_deref(),
                Some(APPLY_METHOD_PENDING_REBOOT),
                "case {group_case:?}: {} is a group's own row, got {found:?}",
                found.name
            );
        }
    }
}

/// A report's `Source` selects the parameters it would report UNDER that source: the
/// service-supplied entries under `system`, the rest under `engine-default`, both at once when
/// the request names none, and none of them under `user` until a caller sets one.
#[tokio::test]
async fn a_source_selects_the_parameters_reported_under_it() {
    let router = emulator();

    for (index, family) in MYSQL_FAMILIES.into_iter().enumerate() {
        let case = format!("family {family}");
        let group = create_group(
            &router,
            &case,
            &format!("emu-source-filter-{index}"),
            family,
        )
        .await;

        let every = names(&listed(&router, &case, &group, None).await);
        let supplied = names(
            &listed(&router, &case, &group, Some(PARAMETER_SOURCE_SYSTEM)).await,
        );
        let engine = names(
            &listed(&router, &case, &group, Some(PARAMETER_SOURCE_ENGINE_DEFAULT)).await,
        );
        let callers = names(&listed(&router, &case, &group, Some(PARAMETER_SOURCE_USER)).await);

        for (_, name, source, _) in SOURCED {
            let selected = if source == PARAMETER_SOURCE_SYSTEM {
                &supplied
            } else {
                &engine
            };
            assert!(
                selected.iter().any(|reported| reported == name),
                "case {case:?}: a Source of {source} selects {name}, got {selected:?}"
            );
            let excluded = if source == PARAMETER_SOURCE_SYSTEM {
                &engine
            } else {
                &supplied
            };
            assert!(
                !excluded.iter().any(|reported| reported == name),
                "case {case:?}: a Source other than {source} must not select {name}, got \
                 {excluded:?}"
            );
        }

        assert!(
            !supplied.is_empty(),
            "case {case:?}: this vocabulary publishes entries the managed service supplies \
             the value of, got {supplied:?}"
        );
        assert_eq!(
            supplied.len() + engine.len(),
            every.len(),
            "case {case:?}: the two published sources partition the family's list, got \
             {supplied:?} and {} engine-sourced rows against {} in all",
            engine.len(),
            every.len()
        );
        assert_eq!(
            callers,
            Vec::<String>::new(),
            "case {case:?}: nobody has set a parameter of this group, got {callers:?}"
        );
    }

    // The complement: a vocabulary whose entries are all the engine's own settles every row
    // under the engine's source, so the service-supplied selection is empty.
    let case = format!("family {ENGINE_ONLY_FAMILY}");
    let group = create_group(&router, &case, "emu-source-filter-engine", ENGINE_ONLY_FAMILY).await;
    let supplied = names(&listed(&router, &case, &group, Some(PARAMETER_SOURCE_SYSTEM)).await);
    assert_eq!(
        supplied,
        Vec::<String>::new(),
        "case {case:?}: this vocabulary publishes no value of the service's own, got \
         {supplied:?}"
    );
}

/// The names of some reported rows, in the order they were reported.
fn names(rows: &[Reported]) -> Vec<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

/// The published domain is what a write is held to, at each published bound and one step past
/// it — including an upper bound past the largest signed 64-bit integer — and a value a group
/// sets is then reported as the CALLER's rather than under the source the default came from.
#[tokio::test]
async fn the_published_domain_bounds_a_write_whatever_its_magnitude() {
    let router = emulator();
    let family = "mysql8.0";
    let case = format!("family {family}");
    let group = create_group(&router, &case, "emu-wide-domain", family).await;

    // The domain the report publishes for the parameter is the engine's own.
    let defaults = published_defaults(&router, &case, family).await;
    let published = row(&case, &defaults, RANGED_PARAMETER);
    assert_eq!(
        published.allowed_values.as_deref(),
        Some(RANGED_DOMAIN),
        "case {case:?}: {RANGED_PARAMETER} publishes the engine's own domain, got \
         {published:?}"
    );

    for (row_case, value, admitted) in DOMAIN {
        let case = format!("{family}: {row_case} ({value})");
        let answered = modify_in(
            &router,
            REGION,
            &group,
            &[(RANGED_PARAMETER, value, Some(APPLY_METHOD_PENDING_REBOOT))],
        )
        .await;
        if !admitted {
            let refused = expect_error(&case, answered, 400, INVALID_PARAMETER_VALUE);
            assert!(
                refused.contains(RANGED_PARAMETER),
                "case {case:?}: the refusal names the parameter it is about, got {refused}"
            );
            continue;
        }
        expect_ok(&case, answered);
        let stored = listed(&router, &case, &group, Some(PARAMETER_SOURCE_USER)).await;
        let found = row(&case, &stored, RANGED_PARAMETER);
        assert_eq!(
            (found.value.as_deref(), found.source.as_deref()),
            (Some(value), Some(PARAMETER_SOURCE_USER)),
            "case {case:?}: a value the caller set is read back as the caller's, got {found:?}"
        );
    }
}
