// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RECORD every row of a `DescribeDBParameters` report carries.
//!
//! RULE: every `Parameter` a DB (instance) parameter group reports carries the attributes its
//! `DBParameterGroupFamily`'s published parameter catalogue holds for that parameter —
//! `Description`, `ApplyType`, `DataType`, `AllowedValues`, `IsModifiable` and
//! `MinimumEngineVersion` — whichever value the row carries: the engine's own default or one a
//! caller wrote with `ModifyDBParameterGroup`. The written value, its `Source` and its
//! `ApplyMethod` are the group's; everything else on the row is the family's catalogue, so a
//! client can read what a parameter accepts off any row of any page.
//!
//! The sibling suites own the neighbouring halves: `db_parameter_metadata` the record a FRESH
//! group publishes for each parameter of its family, `engine_default_parameters` the
//! family-level report of the same catalogue, and `db_parameters` the source filter, the apply
//! method and the value rule. What is pinned HERE is the row a caller has WRITTEN — the half
//! the group's own state feeds — and that every page of a paged report carries the same
//! record.
//!
//! Each rule is asserted over a TABLE spanning what it generalises over: the engine
//! vocabulary and several families per engine, every modifiable parameter of each of those
//! catalogues (so every data type, apply type and published-domain form in them), and the page
//! size at both published bounds and one in between.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    family_parameters, published_db_parameters, published_spec_for_family, ClusterParameterSpec,
    MAX_PARAMETERS_PER_GROUP, MAX_PARAMETERS_PER_MODIFY, MAX_RECORDS_RANGE, PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{emulator, expect_error, expect_ok, optional_text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};
use crate::engine_default_parameters::{reported, Reported};

/// The error code Amazon RDS answers a parameter value it cannot serve with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One family per engine vocabulary this front door publishes a DB-parameter catalogue for,
/// plus a second family of two of those engines — the parameters belong to the ENGINE, so
/// another release line of it publishes the same records.
///
/// The Microsoft SQL Server configuration options carry SPACES in their names and the
/// DocumentDB catalogue publishes a value carrying a `+`, both of which the shared awsQuery
/// test sender refuses to form-encode, so those two vocabularies are driven by the suites that
/// own them.
const FAMILIES: [&str; 7] = [
    "mysql8.0",
    "mysql5.7",
    "mariadb10.11",
    "postgres16",
    "aurora-postgresql16",
    "oracle-ee-19",
    "neptune1.3",
];

/// The attributes of one reported row that come from the family's catalogue rather than from
/// the group: `(Description, ApplyType, DataType, AllowedValues, IsModifiable,
/// MinimumEngineVersion)`.
type Catalogued = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Those attributes as the response reported them.
fn catalogued(entry: &Reported) -> Catalogued {
    (
        entry.description.clone(),
        entry.apply_type.clone(),
        entry.data_type.clone(),
        entry.allowed_values.clone(),
        entry.is_modifiable.clone(),
        entry.minimum_engine_version.clone(),
    )
}

/// Those attributes as the family `family` publishes them for `spec`.
///
/// An EMPTY catalogue column is one the engine publishes nothing for, and the matching member
/// is then left out rather than reported empty: a parameter the published list carries no
/// explanatory text for has no `Description`, one the engine bounds by nothing has no
/// `AllowedValues`, and one the engine has carried since before any version this service
/// serves names no `MinimumEngineVersion`.
fn published(family: &str, spec: &ClusterParameterSpec) -> Catalogued {
    let resolved = published_spec_for_family(family, spec);
    let column = |value: &str| (!value.is_empty()).then(|| value.to_string());
    (
        column(resolved.description),
        Some(resolved.apply_type.to_string()),
        Some(resolved.data_type.to_string()),
        column(resolved.allowed_values),
        Some(resolved.is_modifiable.to_string()),
        column(resolved.minimum_engine_version),
    )
}

/// A value `spec` admits, for a test that needs ONE: the first value of a published
/// enumeration, the low bound of a published numeric range, the single value a parameter
/// publishes, or the engine's own default where the parameter publishes no domain at all.
///
/// The engine's default is the LAST resort rather than the first, because a default the
/// service computes from the instance class is published as the FORMULA Amazon RDS reports
/// (`{DBInstanceClassMemory*3/4}`) rather than as a number, and a formula is outside the
/// numeric range the same catalogue entry publishes.
///
/// This is deliberately not the bounds-and-one-past generator the value rule is driven by
/// (`db_parameters`): what is wanted here is a value that lands, so the row it writes can be
/// read back.
fn an_admitted_value(spec: &ClusterParameterSpec) -> String {
    if spec.allowed_values.contains(',') {
        return spec
            .allowed_values
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    let numeric = matches!(spec.data_type, "integer" | "float");
    if let Some((low, _)) = spec.range().filter(|_| numeric) {
        return low.to_string();
    }
    if !spec.allowed_values.is_empty() {
        return spec.allowed_values.to_string();
    }
    spec.default_value.to_string()
}

/// One page of `group`'s parameter report: the rows it carried and the `Marker` it answered.
async fn page(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    extra: &[(&str, String)],
) -> (Vec<Reported>, Option<String>) {
    let body = expect_ok(case, describe(router, group, extra).await);
    (reported(&body), optional_text(&body, "Marker"))
}

/// Every row of `group`'s report for `source`, paged to the end with `max_records` where the
/// request names one, plus the markers the pages answered.
///
/// The sibling suites walk a parameter report too (`db_parameters` across one marker,
/// `db_parameter_metadata` to the end of an unnarrowed report), but neither drives the page
/// SIZE or hands back the token each page answered, which is what the paging rule below is
/// asserted over — hence this walker rather than a call into one of theirs.
async fn every_row(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    source: Option<&str>,
    max_records: Option<usize>,
) -> (Vec<Reported>, Vec<Option<String>>) {
    let mut rows: Vec<Reported> = Vec::new();
    let mut markers: Vec<Option<String>> = Vec::new();
    let mut marker: Option<String> = None;
    // A group carries at most `MAX_PARAMETERS_PER_GROUP` parameters and a page at least the
    // published minimum, so the walk is bounded; the loop is capped so a marker that never
    // clears fails rather than hangs.
    let pages = MAX_PARAMETERS_PER_GROUP / (*MAX_RECORDS_RANGE.start() as usize) + 2;
    for _ in 0..pages {
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some(source) = source {
            extra.push(("Source", source.to_string()));
        }
        if let Some(max_records) = max_records {
            extra.push(("MaxRecords", max_records.to_string()));
        }
        if let Some(marker) = &marker {
            extra.push(("Marker", marker.clone()));
        }
        let (page_rows, next) = page(router, case, group, &extra).await;
        if let Some(max_records) = max_records {
            assert!(
                page_rows.len() <= max_records,
                "case {case:?}: a page carries at most the MaxRecords the request asked for, \
                 got {} rows",
                page_rows.len()
            );
        }
        rows.extend(page_rows);
        markers.push(next.clone());
        marker = next;
        if marker.is_none() {
            return (rows, markers);
        }
    }
    panic!("case {case:?}: the parameter report of {group} never finished paging");
}

/// Write `settings` into `group`, in requests of at most the documented per-request maximum.
async fn write_all(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    settings: &[(String, String)],
) {
    for chunk in settings.chunks(MAX_PARAMETERS_PER_MODIFY) {
        let batch: Vec<(&str, &str, Option<&str>)> = chunk
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str(), None))
            .collect();
        expect_ok(case, modify_in(router, REGION, group, &batch).await);
    }
}

/// The modifiable parameters of `family`'s published catalogue, each with a value it admits —
/// what a caller can legitimately write on a group of that family, which is the table the rows
/// below are driven by.
fn writable(family: &str) -> Vec<(&'static ClusterParameterSpec, String)> {
    let catalog = family_parameters(
        family,
        published_db_parameters(family)
            .unwrap_or_else(|| panic!("case {family:?}: this family must publish a catalogue")),
    );
    catalog
        .into_iter()
        .filter(|spec| spec.is_modifiable)
        .take(MAX_PARAMETERS_PER_GROUP)
        .map(|spec| (spec, an_admitted_value(spec)))
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 1: a row the caller wrote still carries the family's catalogue record
// ---------------------------------------------------------------------------

/// For every family in the table, every parameter a caller can write is reported back under
/// the caller's value, `Source` `user` and an apply method — AND under the whole record its
/// family publishes for it.
#[tokio::test]
async fn a_user_written_row_reports_its_familys_published_catalogue_record() {
    let router = emulator();

    for (row, family) in FAMILIES.into_iter().enumerate() {
        let written = writable(family);
        assert!(
            !written.is_empty(),
            "case {family:?}: this family must publish parameters a caller can set"
        );
        let group = create_group(
            &router,
            family,
            &format!("catalogued-{row}-{}", family.replace(['.', '_'], "-")),
            family,
        )
        .await;
        let settings: Vec<(String, String)> = written
            .iter()
            .map(|(spec, value)| (spec.name.to_string(), value.clone()))
            .collect();
        write_all(&router, family, &group, &settings).await;

        let (rows, _) = every_row(&router, family, &group, Some(PARAMETER_SOURCE_USER), None).await;
        let reported_names: Vec<&str> = rows.iter().map(|entry| entry.name.as_str()).collect();
        let mut expected_names: Vec<&str> = written.iter().map(|(spec, _)| spec.name).collect();
        expected_names.sort_unstable();
        assert_eq!(
            reported_names, expected_names,
            "case {family:?}: a user-scoped read reports every parameter the caller wrote, in \
             name order"
        );

        for (spec, value) in &written {
            let case = format!("{family} {}", spec.name);
            let entry = rows
                .iter()
                .find(|entry| entry.name == spec.name)
                .unwrap_or_else(|| panic!("case {case:?}: expected the row to be reported"));
            assert_eq!(
                catalogued(entry),
                published(family, spec),
                "case {case:?}: expected the family's published record on the row, got {entry:?}"
            );
            assert_eq!(
                entry.value.as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected the value the caller wrote, got {:?}",
                entry.value
            );
            assert_eq!(
                entry.source.as_deref(),
                Some(PARAMETER_SOURCE_USER),
                "case {case:?}: a row a caller wrote is sourced {PARAMETER_SOURCE_USER}, got {:?}",
                entry.source
            );
            assert!(
                entry.apply_method.is_some(),
                "case {case:?}: every parameter of a group carries an apply method, got {:?}",
                entry.apply_method
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: every PAGE of the report carries the same record
// ---------------------------------------------------------------------------

/// A group whose parameters span several pages reports the catalogue record on every row of
/// every page, pages in parameter-name order without repeating or dropping a row, and answers
/// a `Marker` a client passes back rather than one it can read as a parameter name.
///
/// The page size is driven at both published bounds (`MaxRecords` "Minimum 20, maximum 100")
/// and at one size in between, so the rule is asserted over the page boundary falling in
/// different places rather than over one paging.
#[tokio::test]
async fn every_page_of_a_groups_parameter_report_carries_the_catalogue_record() {
    let router = emulator();
    let family = "mysql8.0";
    let written = writable(family);
    let settings: Vec<(String, String)> = written
        .iter()
        .map(|(spec, value)| (spec.name.to_string(), value.clone()))
        .collect();
    assert!(
        settings.len() > *MAX_RECORDS_RANGE.start() as usize,
        "case {family:?}: the catalogue must be longer than one minimum-sized page for this \
         rule to be about paging, got {} parameters",
        settings.len()
    );
    let mut expected_names: Vec<&str> = written.iter().map(|(spec, _)| spec.name).collect();
    expected_names.sort_unstable();

    let group = create_group(&router, family, "catalogued-pages", family).await;
    write_all(&router, family, &group, &settings).await;

    let low = *MAX_RECORDS_RANGE.start() as usize;
    let high = *MAX_RECORDS_RANGE.end() as usize;
    for max_records in [low, low + (high - low) / 2, high] {
        let case = format!("{family} paged at MaxRecords {max_records}");
        let (rows, markers) = every_row(
            &router,
            &case,
            &group,
            Some(PARAMETER_SOURCE_USER),
            Some(max_records),
        )
        .await;

        let names: Vec<&str> = rows.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names, expected_names,
            "case {case:?}: paging the report must walk every parameter once, in name order"
        );
        for (spec, _) in &written {
            let entry = rows
                .iter()
                .find(|entry| entry.name == spec.name)
                .unwrap_or_else(|| panic!("case {case:?}: expected {} to be reported", spec.name));
            assert_eq!(
                catalogued(entry),
                published(family, spec),
                "case {case:?}: every row of every page carries the family's published record, \
                 got {entry:?} for {}",
                spec.name
            );
        }

        // Every page but the last answers a continuation token, and the last answers none.
        let (last, earlier) = markers
            .split_last()
            .unwrap_or_else(|| panic!("case {case:?}: the walk must have read at least one page"));
        assert_eq!(
            last, &None,
            "case {case:?}: the final page owes no Marker, got {last:?}"
        );
        assert!(
            rows.len() <= max_records || !earlier.is_empty(),
            "case {case:?}: a report longer than the page size must answer a Marker, got {} \
             rows over {} pages",
            rows.len(),
            markers.len()
        );
        for marker in earlier {
            let marker = marker
                .as_deref()
                .unwrap_or_else(|| panic!("case {case:?}: a continued page answers a Marker"));
            assert!(
                !names.contains(&marker),
                "case {case:?}: the Marker is an opaque token a client passes back, not a \
                 parameter name, got {marker:?}"
            );
            assert!(
                marker
                    .chars()
                    .all(|char| char.is_ascii_alphanumeric() || "+/=".contains(char)),
                "case {case:?}: the Marker is an opaque encoded token, got {marker:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3: named rows of the published domain, with the bounds a write is held to
// ---------------------------------------------------------------------------

/// Named rows of the same rule, written as LITERALS so a drift in the catalogue fails this
/// test rather than moving with it.
///
/// `(parameter, apply type, data type, allowed values, description)`. The values are the
/// records the MySQL engine's own configuration documentation publishes for these parameters,
/// which is what Amazon RDS reports as `Parameter$ApplyType` / `$DataType` / `$AllowedValues` /
/// `$Description` for the `mysql8.0` family. `host_cache_size`, `innodb_compression_level` and
/// `innodb_fill_factor` are the parameters the recorded case read; `completion_type`,
/// `div_precision_increment` and `innodb_change_buffer_max_size` are parameters it never
/// mentioned.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// The named rows; see [`Row`].
const NAMED_ROWS: [Row; 6] = [
    (
        "completion_type",
        "dynamic",
        "integer",
        "0,1,2",
        "The transaction completion type.",
    ),
    (
        "div_precision_increment",
        "dynamic",
        "integer",
        "0-30",
        "The number of digits by which to increase the scale of the result of division \
         operations performed with the / operator.",
    ),
    (
        "host_cache_size",
        "dynamic",
        "integer",
        "0-65536",
        "The size of the internal host cache.",
    ),
    (
        "innodb_change_buffer_max_size",
        "dynamic",
        "integer",
        "0-50",
        "Maximum size for the InnoDB change buffer, as a percentage of the total size of the \
         buffer pool.",
    ),
    (
        "innodb_compression_level",
        "dynamic",
        "integer",
        "0-9",
        "Specifies the level of zlib compression to use for InnoDB compressed tables and \
         indexes.",
    ),
    (
        "innodb_fill_factor",
        "dynamic",
        "integer",
        "10-100",
        "Defines the percentage of space on each B-tree page that is filled during a sorted \
         index build, with the remaining space reserved for future index growth.",
    ),
];

/// Each named parameter is reported with the record above once a caller has written it, and a
/// write is held to the published domain: each bound of the range is admitted and one step
/// past either end is refused, naming the parameter.
#[tokio::test]
async fn a_named_parameter_reports_its_published_record_and_bounds_a_write() {
    let router = emulator();
    let family = "mysql8.0";
    let group = create_group(&router, "named rows", "catalogued-named", family).await;

    for (name, apply_type, data_type, allowed_values, description) in NAMED_ROWS {
        let (low, high) = allowed_values
            .split_once('-')
            .map(|(low, high)| {
                (
                    low.parse::<i64>().expect("an integer range bound"),
                    high.parse::<i64>().expect("an integer range bound"),
                )
            })
            .unwrap_or_else(|| {
                // An enumerated domain: its own first and last published values stand in for
                // the bounds, and one past them is a number the enumeration does not carry.
                let values: Vec<i64> = allowed_values
                    .split(',')
                    .map(|value| value.parse::<i64>().expect("an enumerated integer"))
                    .collect();
                (
                    *values.first().expect("a non-empty enumeration"),
                    *values.last().expect("a non-empty enumeration"),
                )
            });

        for bound in [low, high] {
            let case = format!("{name} = {bound}");
            expect_ok(
                &case,
                modify_in(&router, REGION, &group, &[(name, &bound.to_string(), None)]).await,
            );
            let (rows, _) = page(
                &router,
                &case,
                &group,
                &[
                    ("Source", PARAMETER_SOURCE_USER.to_string()),
                    ("Filters.Filter.1.Name", "parameter-name".to_string()),
                    ("Filters.Filter.1.Values.Value.1", name.to_string()),
                ],
            )
            .await;
            let entry = rows
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("case {case:?}: expected the row to be reported"));
            assert_eq!(
                (
                    entry.value.as_deref(),
                    entry.apply_type.as_deref(),
                    entry.data_type.as_deref(),
                    entry.allowed_values.as_deref(),
                    entry.description.as_deref(),
                    entry.is_modifiable.as_deref(),
                ),
                (
                    Some(bound.to_string().as_str()),
                    Some(apply_type),
                    Some(data_type),
                    Some(allowed_values),
                    Some(description),
                    Some("true"),
                ),
                "case {case:?}: expected the published record of {name} on the row, got {entry:?}"
            );
        }

        for past in [i128::from(low) - 1, i128::from(high) + 1] {
            let case = format!("{name} = {past} (past the published domain)");
            let answered = expect_error(
                &case,
                modify_in(&router, REGION, &group, &[(name, &past.to_string(), None)]).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            assert!(
                answered.contains(name),
                "case {case:?}: the refusal must name the parameter, got {answered}"
            );
        }
    }
}
