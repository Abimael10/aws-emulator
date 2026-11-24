// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the published domain of a parameter whose upper bound is past the
//! largest SIGNED 64-bit integer, and for the published text of the row that carries it.
//!
//! RULE: `Parameter$AllowedValues` and `Parameter$Description` are columns of the engine
//! parameter list the row's `DBParameterGroupFamily` publishes, so a report carries that
//! list's own values whatever their magnitude or wording: the MySQL-compatible engines count
//! a size in bytes and a connection-error tally in an UNSIGNED 64-bit word, so the upper
//! bound such a parameter publishes is the largest unsigned 64-bit value (or the lower figure
//! the engine states for the variable, as the binary-log caches' is), never the largest
//! SIGNED one, which is half of it; and the description is the published sentence byte for
//! byte rather than a summary of what the setting does. A bound past the signed range is
//! carried as the published string and compared as a wide integer, so a write at the
//! published bound is admitted and one step past it is refused.
//!
//! The sibling suites own the neighbouring halves: `db_parameter_catalog_text` the FORM a
//! numeric domain is written in and the text of rows inside the signed range,
//! `published_parameter_value_sources` the source a published value comes from and the
//! binary-log cache domain, and `db_parameter_catalog_rows` that every page of a report
//! carries the family's whole record. What is pinned HERE is the magnitude of a published
//! bound, on every family that publishes it and on both halves of a report.
//!
//! The tables are LITERALS, so a drift in the catalog fails these tests rather than moving
//! with them, and they span what the rule generalises over: four families of the one MySQL
//! vocabulary, five parameters whose domain reaches the engine's unsigned 64-bit maximum,
//! two whose published maximum is the lower figure the engine states (rows no issue cited),
//! both ends of each domain and one step past the top, and three published sentences — one
//! that names the engine in lower case and ends without a period, one that ends with one and
//! one of a parameter no issue cited.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    family_parameters, published_db_parameters, PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};
use crate::engine_default_parameters::{reported, Reported};

/// The family-level report of the same catalog a group's report reads its columns from.
const ENGINE_DEFAULTS_ACTION: &str = "DescribeEngineDefaultParameters";

/// The error code Amazon RDS answers a parameter value outside a published domain with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The largest SIGNED 64-bit integer, written as a published bound would write it. It is half
/// of the unsigned 64-bit maximum and is no bound these engines' variables have, so it is the
/// value a truncated domain would end at.
const SIGNED_64_BIT_MAXIMUM: &str = "9223372036854775807";

/// The families that share the one MySQL parameter vocabulary: two release lines of the
/// engine itself, the Aurora engine that runs it, and MariaDB, which runs it under its own
/// release numbering. One vocabulary, so each of them publishes the same domains.
const MYSQL_FAMILIES: [&str; 4] = ["mysql5.7", "mysql8.0", "aurora-mysql8.0", "mariadb10.11"];

/// One domain row: the parameter and the `AllowedValues` its family publishes for it.
type Domain = (&'static str, &'static str);

/// The published domains whose upper bound is past the largest signed 64-bit integer.
///
/// Source: the engine parameter list these families publish, whose bounds are the MySQL
/// manual's "Server System Variables" minimum and 64-bit maximum for the variable — a buffer
/// pool from 5 MiB, a connection-error tally from one error, a MEMORY table from 16384 bytes,
/// a sort buffer from 32768 bytes and an in-memory temporary table from 1024 bytes, each up
/// to 18446744073709551615, the largest value the unsigned 64-bit word the engine counts it
/// in holds. The last two rows are the binary-log caches, which no issue cited and whose
/// published maximum is the lower figure the manual states for them: the bound a report
/// carries is the published one, not a round number of the word's own.
const WIDE_DOMAINS: [Domain; 7] = [
    ("innodb_buffer_pool_size", "5242880-18446744073709551615"),
    ("max_connect_errors", "1-18446744073709551615"),
    ("max_heap_table_size", "16384-18446744073709551615"),
    ("sort_buffer_size", "32768-18446744073709551615"),
    ("tmp_table_size", "1024-18446744073709551615"),
    ("binlog_cache_size", "4096-18446744073709547520"),
    ("binlog_stmt_cache_size", "4096-18446744073709547520"),
];

/// One text row: the parameter and the `Description` its family publishes for it.
type Text = (&'static str, &'static str);

/// The published sentences under test, byte for byte.
///
/// Source: the same published parameter list. The first row is the sentence that list gives
/// the InnoDB buffer-pool size — it names the engine in lower case and ends without a period,
/// as the list writes it. The other two are rows no issue cited: one sentence that ends
/// without a period and one that ends with one.
const PUBLISHED_TEXT: [Text; 3] = [
    (
        "innodb_buffer_pool_size",
        "The size in bytes of the memory buffer innodb uses to cache data and indexes of its \
         tables",
    ),
    (
        "innodb_autoextend_increment",
        "The increment size (in MB) for extending the size of an auto-extending tablespace \
         file when it becomes full",
    ),
    (
        "innodb_buffer_pool_dump_pct",
        "Specifies the percentage of the most recently used pages for each buffer pool to \
         read out and dump.",
    ),
];

/// Every row of `group`'s report, paged to the end. `extra` carries any further request
/// members, a `Source` among them.
async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    extra: &[(&str, String)],
) -> Vec<Reported> {
    let mut rows: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    // A published catalog is a few hundred rows at most and a page carries a hundred, so the
    // walk is bounded; the cap makes a marker that never clears fail rather than hang.
    for _ in 0..64 {
        let mut params: Vec<(&str, String)> = extra.to_vec();
        if let Some(marker) = &marker {
            params.push(("Marker", marker.clone()));
        }
        let body = expect_ok(case, describe(router, group, &params).await);
        rows.extend(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return rows;
        }
    }
    panic!("case {case:?}: the parameter report of {group} never finished paging");
}

/// Every row the family-level report of `family` carries, paged to the end.
async fn published_defaults(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
) -> Vec<Reported> {
    let mut rows: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..64 {
        let mut params: Vec<(&str, String)> =
            vec![("DBParameterGroupFamily", family.to_string())];
        if let Some(marker) = &marker {
            params.push(("Marker", marker.clone()));
        }
        let body = expect_ok(case, call(router, ENGINE_DEFAULTS_ACTION, &params).await);
        rows.extend(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return rows;
        }
    }
    panic!("case {case:?}: the engine defaults of {family} never finished paging");
}

/// The row named `name`, or a panic naming what was reported instead.
fn row_for<'a>(case: &str, rows: &'a [Reported], name: &str) -> &'a Reported {
    rows.iter().find(|row| row.name == name).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected {name} among the reported parameters, got {:?}",
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>()
        )
    })
}

/// The upper bound of a published `<low>-<high>` domain.
fn high_bound(case: &str, domain: &str) -> u128 {
    let offset = domain
        .get(1..)
        .and_then(|rest| rest.find('-'))
        .unwrap_or_else(|| panic!("case {case:?}: {domain:?} must be a hyphenated range"))
        + 2;
    domain[offset..]
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("case {case:?}: {domain:?} must end in a number"))
}

/// The lower bound of a published `<low>-<high>` domain.
fn low_bound(case: &str, domain: &str) -> u128 {
    let offset = domain
        .get(1..)
        .and_then(|rest| rest.find('-'))
        .unwrap_or_else(|| panic!("case {case:?}: {domain:?} must be a hyphenated range"))
        + 1;
    domain[..offset]
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("case {case:?}: {domain:?} must start with a number"))
}

// ---------------------------------------------------------------------------
// Rule 1: the published bound and the published sentence, whole, on every family
// ---------------------------------------------------------------------------

/// Each parameter whose domain reaches the engine's 64-bit maximum reports that whole domain,
/// and each named row reports its published sentence — in a group's report and in the
/// family-level report of the same catalog alike, on every family that shares the vocabulary.
#[tokio::test]
async fn a_wide_published_domain_and_its_sentence_are_reported_whole() {
    let router = emulator();

    for (index, family) in MYSQL_FAMILIES.into_iter().enumerate() {
        let family_case = format!("family {family}");
        let group = create_group(
            &router,
            &family_case,
            &format!("emu-wide-domain-{index}"),
            family,
        )
        .await;
        let group_rows = rows_of(&router, &family_case, &group, &[]).await;
        let default_rows = published_defaults(&router, &family_case, family).await;
        let levels: [(&str, &Vec<Reported>); 2] = [
            ("a group's report", &group_rows),
            ("the family's engine defaults", &default_rows),
        ];

        for (name, domain) in WIDE_DOMAINS {
            let case = format!("{family}: the domain of {name}");
            for (level, rows) in levels {
                let row = row_for(&case, rows, name);
                assert_eq!(
                    row.allowed_values.as_deref(),
                    Some(domain),
                    "case {case:?}: {level} must publish the engine's whole domain \
                     {domain:?}, got {row:?}"
                );
            }
        }

        for (name, description) in PUBLISHED_TEXT {
            let case = format!("{family}: the published sentence of {name}");
            for (level, rows) in levels {
                let row = row_for(&case, rows, name);
                assert_eq!(
                    row.description.as_deref(),
                    Some(description),
                    "case {case:?}: {level} must publish the list's own sentence \
                     {description:?}, got {row:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the published bound is what a write is held to, whatever its magnitude
// ---------------------------------------------------------------------------

/// Each published bound past the signed 64-bit range is the bound a write is really held to:
/// both ends of the domain are admitted and read back as the caller's own value, and one step
/// past the top is refused in the sentence that names the parameter.
#[tokio::test]
async fn a_bound_past_the_signed_range_admits_its_own_value_and_refuses_the_next() {
    let router = emulator();
    let family = "mysql8.0";
    let group = create_group(&router, "wide bounds", "emu-wide-bounds", family).await;

    for (name, domain) in WIDE_DOMAINS {
        let case = format!("{name} ({domain})");
        let low = low_bound(&case, domain).to_string();
        let high = high_bound(&case, domain);
        let past = (high + 1).to_string();
        let high = high.to_string();

        for value in [low.as_str(), high.as_str()] {
            let case = format!("{case}: the published bound {value}");
            expect_ok(
                &case,
                modify_in(&router, REGION, &group, &[(name, value, None)]).await,
            );
            let written = rows_of(
                &router,
                &case,
                &group,
                &[("Source", PARAMETER_SOURCE_USER.to_string())],
            )
            .await;
            let row = row_for(&case, &written, name);
            assert_eq!(
                (row.value.as_deref(), row.source.as_deref()),
                (Some(value), Some(PARAMETER_SOURCE_USER)),
                "case {case:?}: a value inside the published domain is stored and read back \
                 as the caller's, got {row:?}"
            );
            assert_eq!(
                row.allowed_values.as_deref(),
                Some(domain),
                "case {case:?}: the written row still carries the family's own domain, got \
                 {row:?}"
            );
        }

        let case = format!("{case}: one step past the published bound {past}");
        let refused = expect_error(
            &case,
            modify_in(&router, REGION, &group, &[(name, &past, None)]).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        assert!(
            refused.contains(name) && refused.contains(domain),
            "case {case:?}: the refusal names the parameter and the domain it is about, got \
             {refused}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the whole published vocabulary, not only the rows a report was read for
// ---------------------------------------------------------------------------

/// No integer parameter of the MySQL vocabulary publishes a domain that stops at the largest
/// SIGNED 64-bit integer: these engines count such a setting in an unsigned 64-bit word, so a
/// bound that ends there would publish half the domain the engine has, and every published
/// upper bound fits the word it is counted in.
///
/// Asserted over EVERY entry of every family that shares the vocabulary rather than over the
/// rows above, because the rule is a property of the published list. It is the MySQL
/// vocabulary's rule: an engine whose own settings are signed 64-bit integers may publish
/// that bound honestly, so the sweep is not widened to those catalogs.
#[test]
fn no_published_mysql_domain_stops_at_the_signed_64_bit_maximum() {
    for family in MYSQL_FAMILIES {
        let catalog = published_db_parameters(family)
            .unwrap_or_else(|| panic!("case {family:?}: the family must publish a catalog"));
        let mut ranges = 0_usize;
        for spec in family_parameters(family, catalog) {
            if spec.data_type != "integer" {
                continue;
            }
            let Some((_, high)) = spec.range() else {
                continue;
            };
            ranges += 1;
            assert_ne!(
                high, SIGNED_64_BIT_MAXIMUM,
                "case {family:?}: {} publishes {:?}, whose upper bound is the signed 64-bit \
                 maximum rather than the domain the engine has",
                spec.name, spec.allowed_values
            );
            assert!(
                high.parse::<u64>().is_ok(),
                "case {family:?}: {} publishes the upper bound {high:?}, which is no value \
                 the unsigned 64-bit word the engine counts it in holds",
                spec.name
            );
        }
        assert!(
            ranges > 0,
            "case {family:?}: the vocabulary must publish integer domains written as ranges, \
             got {ranges}"
        );
    }
}
