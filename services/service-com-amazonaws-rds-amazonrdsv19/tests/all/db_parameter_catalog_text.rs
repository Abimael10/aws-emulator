// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TEXT and the FORM of the catalogue columns a
//! `DescribeDBParameters` row carries.
//!
//! RULE: `Parameter$Description`, `Parameter$AllowedValues` and
//! `Parameter$MinimumEngineVersion` are columns of the published parameter list of the row's
//! `DBParameterGroupFamily`, so a report carries that list's own values: the description is
//! the published text for the parameter NAME byte for byte rather than a summary of what the
//! setting does, a numeric domain that is the whole run between two bounds is the hyphenated
//! range `<low>-<high>` rather than an enumeration of the values in it, and a
//! `MinimumEngineVersion` is reported only where the list pins one. The row a CALLER has
//! written carries the same columns as the row the engine's own default feeds: a caller owns
//! the value, its `Source` and its `ApplyMethod`, and the family owns everything else, so a
//! client can read what a parameter means and what it accepts off any row of any page.
//!
//! The sibling suites own the neighbouring halves: `db_parameter_catalog_rows` that a written
//! row carries the family's whole record and that every page carries it, `db_parameter_metadata`
//! the record a fresh group publishes, `engine_default_parameters` the family-level report of
//! the same catalogue, and `refusal_wording_and_input_guards` the refusal each domain FORM is
//! worded by. What is pinned HERE is the content of the two text columns and the form of the
//! domain column.
//!
//! The rules are asserted over a TABLE of named rows — spanning the three-valued integer
//! settings, the per-engine `innodb_*` and `information_schema_*` settings, parameters whose
//! published text ends without a period and parameters the recorded case never read — and,
//! for the domain FORM, over EVERY integer parameter of EVERY family this front door publishes
//! a catalogue for, which is where the rule holds rather than only on the rows a report was
//! once read for.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    family_parameters, published_db_parameters, PARAMETER_SOURCE_ENGINE_DEFAULT,
    PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{emulator, expect_ok, optional_text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};
use crate::engine_default_parameters::{reported, Reported};

/// The family the named rows below are published by: one release line of the MySQL-compatible
/// engines, whose published list carries every parameter they name.
const FAMILY: &str = "mysql8.0";

/// One named row: the parameter, the published domain its family reports under
/// `AllowedValues`, the published text it reports under `Description`, and a value inside that
/// domain a caller may write.
type Row = (&'static str, &'static str, &'static str, &'static str);

/// The named rows, written as LITERALS so a drift in the catalogue fails this test rather than
/// moving with it.
///
/// Source: the published parameter list of the `mysql8.0` family — the same list
/// `DescribeEngineDefaultParameters` reports for it — which is what Amazon RDS reports as
/// `Parameter$Description` and `Parameter$AllowedValues` for a group of that family. The rows
/// span what the rule generalises over: the three-valued integer settings whose domain is a
/// RUN (`completion_type`, `concurrent_insert`, `innodb_flush_log_at_trx_commit` and
/// `innodb_flush_neighbors`, the last two of which the recorded case never read), published
/// text that ends WITHOUT a period (`innodb_autoextend_increment`,
/// `innodb_lock_wait_timeout`), text that names the engine in lower case
/// (`innodb_lock_wait_timeout`), a long multi-clause sentence
/// (`innodb_compression_pad_pct_max`), and an entry whose published text is the sentence a
/// neighbouring entry of the list also carries (`innodb_compression_level`, which the list
/// describes with the padding-threshold sentence).
const NAMED_ROWS: [Row; 12] = [
    (
        "completion_type",
        "0-2",
        "The transaction completion type (0 - default, 1 - chain, 2 - release)",
        "2",
    ),
    (
        "concurrent_insert",
        "0-2",
        "Allows INSERT and SELECT statements to run concurrently for MyISAM tables that have \
         no free blocks in the middle of the data file.",
        "2",
    ),
    (
        "innodb_flush_log_at_trx_commit",
        "0-2",
        "Controls the balance between strict ACID compliance for commit operations and the \
         higher performance of flushing the log less often.",
        "2",
    ),
    (
        "innodb_flush_neighbors",
        "0-2",
        "Specifies whether flushing a page from the InnoDB buffer pool also flushes other \
         dirty pages in the same extent.",
        "1",
    ),
    (
        "information_schema_stats_expiry",
        "0-31536000",
        "The number of seconds after which mysqld server will fetch data from storage engine \
         and replace the data in cache.",
        "100",
    ),
    (
        "innodb_adaptive_flushing_lwm",
        "0-70",
        "Low water mark representing percentage of redo log capacity at which adaptive \
         flushing is enabled.",
        "15",
    ),
    (
        "innodb_autoextend_increment",
        "1-1000",
        "The increment size (in MB) for extending the size of an auto-extending tablespace \
         file when it becomes full",
        "32",
    ),
    (
        "innodb_lock_wait_timeout",
        "1-1073741824",
        "Timeout in seconds an innodb transaction may wait for a row lock before giving up",
        "60",
    ),
    (
        "innodb_compression_failure_threshold_pct",
        "0-100",
        "Sets the cutoff point at which MySQL begins adding padding within compressed pages \
         to avoid expensive compression failures.",
        "5",
    ),
    (
        "innodb_compression_level",
        "0-9",
        "Sets the cutoff point at which MySQL begins adding padding within compressed pages \
         to avoid expensive compression failures.",
        "6",
    ),
    (
        "innodb_compression_pad_pct_max",
        "0-75",
        "Specifies the maximum percentage that can be reserved as free space within each \
         compressed page, allowing room to reorganize the data and modification log within \
         the page when a compressed table or index is updated and the data might be \
         recompressed.",
        "40",
    ),
    (
        "innodb_fill_factor",
        "10-100",
        "Defines the percentage of space on each B-tree page that is filled during a sorted \
         index build, with the remaining space reserved for future index growth.",
        "90",
    ),
];

/// Every row of `group`'s report for `source`, paged to the end.
///
/// The sibling suites' walkers either drive the page size or hand back each page's token,
/// neither of which these rules are about; what is wanted here is the whole report under one
/// source, so the walk asks for no page size and follows the marker it is given.
async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    source: &str,
) -> Vec<Reported> {
    let mut rows: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    // The published catalogue of a family is a few hundred rows at most and a page carries a
    // hundred, so the walk is bounded; the cap makes a marker that never clears fail rather
    // than hang.
    for _ in 0..64 {
        let mut extra: Vec<(&str, String)> = vec![("Source", source.to_string())];
        if let Some(marker) = &marker {
            extra.push(("Marker", marker.clone()));
        }
        let body = expect_ok(case, describe(router, group, &extra).await);
        rows.extend(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return rows;
        }
    }
    panic!("case {case:?}: the parameter report of {group} never finished paging");
}

/// The row `name` in `rows`.
fn row_for<'a>(case: &str, rows: &'a [Reported], name: &str) -> &'a Reported {
    rows.iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry {name}, got {rows:?}"))
}

// ---------------------------------------------------------------------------
// Rule 1: the published text and the published domain, on both halves of a report
// ---------------------------------------------------------------------------

/// Each named parameter reports its family's published `Description` and `AllowedValues`,
/// whichever half of the report the row came from: the engine's own default before any write,
/// and the caller's own value after one.
#[tokio::test]
async fn a_named_parameter_reports_its_published_text_and_domain_on_both_halves() {
    let router = emulator();
    let group = create_group(&router, "published text", "catalogued-text", FAMILY).await;

    let defaults = rows_of(
        &router,
        "the engine-default half",
        &group,
        PARAMETER_SOURCE_ENGINE_DEFAULT,
    )
    .await;
    for (name, allowed_values, description, _) in NAMED_ROWS {
        let case = format!("{name} as an engine default");
        let row = row_for(&case, &defaults, name);
        assert_eq!(
            (row.description.as_deref(), row.allowed_values.as_deref()),
            (Some(description), Some(allowed_values)),
            "case {case:?}: expected the published text and domain \
             ({description:?}, {allowed_values:?}), got {row:?}"
        );
    }

    for (name, _, _, value) in NAMED_ROWS {
        let case = format!("{name} written by a caller");
        expect_ok(
            &case,
            modify_in(&router, REGION, &group, &[(name, value, None)]).await,
        );
    }
    let written = rows_of(&router, "the user half", &group, PARAMETER_SOURCE_USER).await;
    for (name, allowed_values, description, value) in NAMED_ROWS {
        let case = format!("{name} written by a caller");
        let row = row_for(&case, &written, name);
        assert_eq!(
            (
                row.value.as_deref(),
                row.source.as_deref(),
                row.description.as_deref(),
                row.allowed_values.as_deref()
            ),
            (
                Some(value),
                Some(PARAMETER_SOURCE_USER),
                Some(description),
                Some(allowed_values)
            ),
            "case {case:?}: a written row carries the caller's value under the family's own \
             text and domain, got {row:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the FORM of a numeric domain, over every family's whole catalogue
// ---------------------------------------------------------------------------

/// One family per engine vocabulary this front door publishes a DB-parameter catalogue for,
/// plus a second release line of two of those engines — the published domains belong to the
/// ENGINE, so another line of it publishes them in the same form.
///
/// The Microsoft SQL Server configuration options carry SPACES in their names, which the
/// shared awsQuery test sender refuses to form-encode, so that vocabulary is driven by the
/// suite that owns it.
const FAMILIES: [&str; 7] = [
    "mysql8.0",
    "mysql5.7",
    "mariadb10.11",
    "postgres16",
    "aurora-postgresql16",
    "oracle-ee-19",
    "neptune1.3",
];

/// Whether `domain` is an enumeration of CONSECUTIVE integers (`0,1,2`) — the form a published
/// run is NOT written in.
fn is_a_run_written_as_an_enumeration(domain: &str) -> bool {
    let values: Vec<i128> = domain
        .split(',')
        .map(|value| value.trim().parse::<i128>())
        .collect::<Result<Vec<i128>, _>>()
        .unwrap_or_default();
    values.len() > 1 && values.windows(2).all(|pair| pair[1] == pair[0] + 1)
}

/// No integer parameter of any published family reports a contiguous run of values as an
/// enumeration: a run is reported as the hyphenated range between its bounds, so a client that
/// parses `AllowedValues` to decide whether a candidate value is legal reads the same domain
/// Amazon RDS publishes.
///
/// Asserted over every reported row rather than over the named rows above, because the form is
/// a property of the whole published list; a boolean's two values stay the enumeration `0,1`,
/// which is the form that list writes them in.
#[tokio::test]
async fn no_integer_parameter_reports_a_run_of_values_as_an_enumeration() {
    let router = emulator();

    for (index, family) in FAMILIES.into_iter().enumerate() {
        let case = format!("the published integer domains of {family}");
        let group = create_group(
            &router,
            &case,
            &format!("catalogued-domain-{index}"),
            family,
        )
        .await;
        let rows = rows_of(&router, &case, &group, PARAMETER_SOURCE_ENGINE_DEFAULT).await;
        assert!(
            !rows.is_empty(),
            "case {case:?}: {family} must publish a parameter list"
        );
        for row in &rows {
            let Some(domain) = row.allowed_values.as_deref() else {
                continue;
            };
            if row.data_type.as_deref() != Some("integer") {
                continue;
            }
            assert!(
                !is_a_run_written_as_an_enumeration(domain),
                "case {case:?}: {} publishes the run {domain:?} as an enumeration; a \
                 contiguous integer domain is reported as the range between its bounds",
                row.name
            );
        }
    }
}

/// The reported domain of an integer parameter is the form its family's catalogue entry holds,
/// so the report and the catalogue cannot disagree about either the domain or its form.
///
/// This is the same rule as the test above read from the other side: every integer entry of
/// every published family either names a hyphenated range whose bounds parse as numbers, names
/// a set of values that is not a run, or publishes no domain at all.
#[test]
fn every_published_integer_domain_is_a_range_or_a_set_that_is_no_run() {
    for family in FAMILIES {
        let catalogue = published_db_parameters(family)
            .unwrap_or_else(|| panic!("case {family:?}: the family must publish a catalogue"));
        for spec in family_parameters(family, catalogue) {
            if spec.data_type != "integer" || spec.allowed_values.is_empty() {
                continue;
            }
            assert!(
                !is_a_run_written_as_an_enumeration(spec.allowed_values),
                "case {family:?}: {} publishes the run {:?} as an enumeration",
                spec.name,
                spec.allowed_values
            );
            if spec.allowed_values.contains(',') {
                continue;
            }
            assert!(
                spec.range().is_some() || spec.allowed_values.parse::<i128>().is_ok(),
                "case {family:?}: {} publishes {:?}, which is neither a range nor a value",
                spec.name,
                spec.allowed_values
            );
        }
    }
}
