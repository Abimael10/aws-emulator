// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a parameter group's MEMBERSHIP is, and for the published record
//! every row of a parameter report carries.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper this
//! test target has, as a real awsQuery form POST carrying the SigV4 credential scope the router
//! keys on, so every assertion reads the XML the emulator itself produced.
//!
//! The rules under test:
//!
//! 1. A parameter group carries its family's ENGINE parameters and no others. A
//!    `ParameterName` the engine of the group's family has no setting for names no parameter of
//!    the group, so the write that would set it and the reset that would clear it are both
//!    refused with `InvalidParameterValue` and the published not-found wording, and no read —
//!    narrowed by `Source` or not — reports it. A name the engine HAS is carried whether or not
//!    this front door publishes a record for it, and a family whose engine's namespace this
//!    door does not carry admits any name: the refusal rests on the engine's own namespace.
//! 2. Every `Parameter` a report projects carries the published record of that parameter —
//!    `Description`, `ApplyType`, `DataType`, `AllowedValues` and `IsModifiable` — on a row the
//!    CALLER set (`Source` `user`) exactly as on the engine's own default row, because the
//!    record belongs to the parameter and not to the value's source. The row also carries the
//!    value and the apply method the caller asked for.
//! 3. A read narrowed to the caller's own parameters reports EVERY parameter the group carries,
//!    each with that record and with its own apply method, so reading back what one changed
//!    reports the change rather than an empty group.
//!
//! Each rule is asserted over a TABLE spanning what it generalises over: both LEVELS (a DB
//! cluster parameter group and a DB parameter group), both engine namespaces this door carries
//! (the PostgreSQL- and the MySQL-compatible engines) plus families of engines it carries none
//! for, every `DataType` a published record names (float, integer, boolean, string), both apply
//! methods the model's `ApplyMethod` enum names, both forms of the read (no `Source`, and
//! `Source` naming the caller's own parameters), and both name forms the engines admit (a plain
//! setting name, and the dotted customized option of a PostgreSQL extension).

use service_com_amazonaws_rds_amazonrdsv19::state::{
    engine_has_no_setting_named, published_cluster_parameters, published_db_parameters,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, text};
use crate::engine_default_parameters::{reported, Reported};

/// The Query API code Amazon RDS answers a parameter it cannot serve with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// `Parameter$Source` of a row the caller set.
const SOURCE_USER: &str = "user";

/// The two apply methods the model's `ApplyMethod` enum names.
const IMMEDIATE: &str = "immediate";
const PENDING_REBOOT: &str = "pending-reboot";

/// Which of the two parameter-group levels a row drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    /// A DB CLUSTER parameter group.
    Cluster,
    /// A DB (instance) parameter group.
    Instance,
}

impl Level {
    /// The member a request of this level names the group by.
    fn group_member(self) -> &'static str {
        match self {
            Self::Cluster => "DBClusterParameterGroupName",
            Self::Instance => "DBParameterGroupName",
        }
    }

    /// The operation that creates a group of this level.
    fn create_action(self) -> &'static str {
        match self {
            Self::Cluster => "CreateDBClusterParameterGroup",
            Self::Instance => "CreateDBParameterGroup",
        }
    }

    /// The operation that sets a parameter of a group of this level.
    fn modify_action(self) -> &'static str {
        match self {
            Self::Cluster => "ModifyDBClusterParameterGroup",
            Self::Instance => "ModifyDBParameterGroup",
        }
    }

    /// The operation that puts a parameter of a group of this level back to its default.
    fn reset_action(self) -> &'static str {
        match self {
            Self::Cluster => "ResetDBClusterParameterGroup",
            Self::Instance => "ResetDBParameterGroup",
        }
    }

    /// The operation that reports the parameters of a group of this level.
    fn describe_action(self) -> &'static str {
        match self {
            Self::Cluster => "DescribeDBClusterParameters",
            Self::Instance => "DescribeDBParameters",
        }
    }
}

/// Create a group of `level` in `family` and answer the name the store keys it under.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    level: Level,
    name: &str,
    family: &str,
) -> String {
    expect_ok(
        case,
        call(
            router,
            level.create_action(),
            &[
                (level.group_member().to_string(), name.to_string()),
                (
                    "DBParameterGroupFamily".to_string(),
                    family.to_string(),
                ),
                (
                    "Description".to_string(),
                    "a group under test".to_string(),
                ),
            ],
        )
        .await,
    );
    name.to_ascii_lowercase()
}

/// Set one parameter on `group`, with an optional apply method.
async fn modify(
    router: &MultiServiceRouter,
    level: Level,
    group: &str,
    name: &str,
    value: &str,
    apply_method: Option<&str>,
) -> (u16, String) {
    let mut params: Vec<(String, String)> = vec![
        (level.group_member().to_string(), group.to_string()),
        (
            "Parameters.Parameter.1.ParameterName".to_string(),
            name.to_string(),
        ),
        (
            "Parameters.Parameter.1.ParameterValue".to_string(),
            value.to_string(),
        ),
    ];
    if let Some(method) = apply_method {
        params.push((
            "Parameters.Parameter.1.ApplyMethod".to_string(),
            method.to_string(),
        ));
    }
    call(router, level.modify_action(), &params).await
}

/// Put one parameter of `group` back to its default.
async fn reset(
    router: &MultiServiceRouter,
    level: Level,
    group: &str,
    name: &str,
) -> (u16, String) {
    call(
        router,
        level.reset_action(),
        &[
            (level.group_member().to_string(), group.to_string()),
            (
                "Parameters.Parameter.1.ParameterName".to_string(),
                name.to_string(),
            ),
        ],
    )
    .await
}

/// Every `<Parameter>` a group of `level` reports for `source`, walked across the report's
/// `Marker`s so a row past the first page is read too.
///
/// "Constraints: Minimum 20, maximum 100" is the page the request asks for, and the walk is
/// bounded, because a published catalog is finite: a report that never ends is a failure here
/// rather than a hang.
async fn reported_rows(
    router: &MultiServiceRouter,
    case: &str,
    level: Level,
    group: &str,
    source: Option<&str>,
) -> Vec<Reported> {
    const PAGE_LIMIT: usize = 16;
    let mut rows: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut params: Vec<(String, String)> = vec![
            (level.group_member().to_string(), group.to_string()),
            ("MaxRecords".to_string(), "100".to_string()),
        ];
        if let Some(source) = source {
            params.push(("Source".to_string(), source.to_string()));
        }
        if let Some(marker) = &marker {
            params.push(("Marker".to_string(), marker.clone()));
        }
        let body = expect_ok(case, call(router, level.describe_action(), &params).await);
        rows.extend(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return rows;
        }
    }
    panic!("case {case:?}: the parameter report of {group} never finished paging");
}

/// The row `name` in a report, or `None` where the report does not carry it.
fn row_for(rows: &[Reported], name: &str) -> Option<Reported> {
    rows.iter().find(|row| row.name == name).cloned()
}

/// The five published columns of a reported row, in the order the assertions read them:
/// `(Description, ApplyType, DataType, AllowedValues, IsModifiable)`.
type PublishedRecord = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The published record a reported row carries.
fn published_record(row: &Reported) -> PublishedRecord {
    (
        row.description.clone(),
        row.apply_type.clone(),
        row.data_type.clone(),
        row.allowed_values.clone(),
        row.is_modifiable.clone(),
    )
}

/// The record the ENGINE-DEFAULT door publishes for `name` in `family`: the answer a group's
/// row is held against, so the two doors cannot disagree about one parameter's record.
///
/// A cluster-level group's row is described by whichever level of its family publishes the
/// parameter — the engine's own parameters are published at the instance level and a cluster
/// group of that family carries them too — so the cluster door is asked first and the instance
/// door answers for a parameter it does not publish.
async fn published_by_the_engine_default_door(
    router: &MultiServiceRouter,
    case: &str,
    level: Level,
    family: &str,
    name: &str,
) -> PublishedRecord {
    if level == Level::Cluster {
        let body = expect_ok(
            case,
            call(
                router,
                "DescribeEngineDefaultClusterParameters",
                &[
                    ("DBParameterGroupFamily", family.to_string()),
                    ("MaxRecords", "100".to_string()),
                ],
            )
            .await,
        );
        if let Some(row) = row_for(&reported(&body), name) {
            return published_record(&row);
        }
    }
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeEngineDefaultParameters",
            &[
                ("DBParameterGroupFamily", family.to_string()),
                ("Filters.Filter.1.Name", "parameter-name".to_string()),
                ("Filters.Filter.1.Values.Value.1", name.to_string()),
            ],
        )
        .await,
    );
    let rows = reported(&body);
    let row = row_for(&rows, name).unwrap_or_else(|| {
        panic!("case {case:?}: the engine-default door of {family} must publish {name}, got {rows:?}")
    });
    published_record(&row)
}

// ---------------------------------------------------------------------------
// Rule 2 and 3: the published record on a row the caller set, under either read
// ---------------------------------------------------------------------------

/// One row of the record table: the case, the level, the family, the parameter, the value to
/// set and the apply method to ask for (`None` where the request names none).
type RecordRow = (
    &'static str,
    Level,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// A row a caller SET carries the published record of its parameter, both when the read names
/// no `Source` and when it narrows to the caller's own parameters, and that record is the one
/// the family's engine-default door publishes for the same parameter.
#[tokio::test]
async fn a_row_the_caller_set_carries_its_parameters_published_record() {
    let router = emulator();

    // The table spans both levels, both engine namespaces, every published `DataType`, both
    // apply methods and the absent one.
    let rows: [RecordRow; 8] = [
        (
            "a float parameter of a PostgreSQL family",
            Level::Instance,
            "postgres16",
            "random_page_cost",
            "2.5",
            None,
        ),
        (
            "an integer parameter of a PostgreSQL family, applied at once",
            Level::Instance,
            "postgres16",
            "log_min_duration_statement",
            "1000",
            Some(IMMEDIATE),
        ),
        (
            "a string parameter of a PostgreSQL family",
            Level::Instance,
            "postgres16",
            "log_statement",
            "ddl",
            None,
        ),
        (
            "a boolean parameter of a MySQL family",
            Level::Instance,
            "mysql8.0",
            "autocommit",
            "0",
            Some(IMMEDIATE),
        ),
        (
            "an integer parameter of a MySQL family, waiting for a reboot",
            Level::Instance,
            "mysql8.0",
            "binlog_cache_size",
            "65536",
            Some(PENDING_REBOOT),
        ),
        (
            "the engine's own integer parameter in a cluster group",
            Level::Cluster,
            "aurora-postgresql16",
            "log_min_duration_statement",
            "1000",
            Some(IMMEDIATE),
        ),
        (
            "a cluster-level boolean parameter of a PostgreSQL family",
            Level::Cluster,
            "aurora-postgresql16",
            "rds.logical_replication",
            "1",
            Some(PENDING_REBOOT),
        ),
        (
            "a cluster-level string parameter of a MySQL family",
            Level::Cluster,
            "aurora-mysql8.0",
            "binlog_format",
            "ROW",
            None,
        ),
    ];

    for (index, (case, level, family, parameter, value, apply_method)) in
        rows.into_iter().enumerate()
    {
        let group = create_group(
            &router,
            case,
            level,
            &format!("emu-record-{index}"),
            family,
        )
        .await;
        expect_ok(
            case,
            modify(&router, level, &group, parameter, value, apply_method).await,
        );

        let published =
            published_by_the_engine_default_door(&router, case, level, family, parameter).await;
        assert!(
            published.0.is_some()
                && published.1.is_some()
                && published.2.is_some()
                && published.3.is_some()
                && published.4.is_some(),
            "case {case:?}: the engine-default door must publish the whole record of \
             {parameter}, got {published:?}"
        );

        // Both forms of the read: the whole report, and the one narrowed to the caller's own
        // parameters. Each must carry the same row.
        for source in [None, Some(SOURCE_USER)] {
            let case = format!("{case} (Source {source:?})");
            let rows = reported_rows(&router, &case, level, &group, source).await;
            let row = row_for(&rows, parameter).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the report to carry {parameter}, got {:?}",
                    rows.iter().map(|row| &row.name).collect::<Vec<_>>()
                )
            });
            assert_eq!(
                (
                    row.value.as_deref(),
                    row.source.as_deref(),
                    row.apply_method.as_deref(),
                ),
                (
                    Some(value),
                    Some(SOURCE_USER),
                    // A change the caller asked no method for takes the method its apply
                    // type implies, which the record published below carries.
                    Some(apply_method.unwrap_or_else(|| {
                        if published.1.as_deref() == Some("dynamic") {
                            IMMEDIATE
                        } else {
                            PENDING_REBOOT
                        }
                    })),
                ),
                "case {case:?}: expected the value, source and apply method the caller set, \
                 got {row:?}"
            );
            assert_eq!(
                published_record(&row),
                published,
                "case {case:?}: a row the caller set carries the published record of its \
                 parameter, got {row:?}"
            );
        }
    }
}

/// A read narrowed to the caller's own parameters reports EVERY parameter the group carries,
/// each with its own apply method and its published record — the read a client makes to
/// confirm the change it applied landed.
#[tokio::test]
async fn a_user_scoped_read_reports_every_parameter_the_group_carries() {
    let router = emulator();
    let family = "aurora-postgresql16";
    let case = "a hardened cluster parameter group";
    let group = create_group(&router, case, Level::Cluster, "emu-user-scoped-cpg", family).await;

    // Two changes, differing in the apply method they take: one in force at once and one
    // waiting for a reboot.
    let written: [(&str, &str, &str); 2] = [
        ("log_min_duration_statement", "1000", IMMEDIATE),
        ("rds.logical_replication", "1", PENDING_REBOOT),
    ];
    for (parameter, value, apply_method) in written {
        expect_ok(
            case,
            modify(
                &router,
                Level::Cluster,
                &group,
                parameter,
                value,
                Some(apply_method),
            )
            .await,
        );
    }

    let rows = reported_rows(&router, case, Level::Cluster, &group, Some(SOURCE_USER)).await;
    assert_eq!(
        rows.len(),
        written.len(),
        "case {case:?}: the caller's own parameters are the two it set, got {:?}",
        rows.iter().map(|row| &row.name).collect::<Vec<_>>()
    );
    for (parameter, value, apply_method) in written {
        let row = row_for(&rows, parameter).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {parameter} in the user-scoped read, got {:?}",
                rows.iter().map(|row| &row.name).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            (
                row.value.as_deref(),
                row.source.as_deref(),
                row.apply_method.as_deref()
            ),
            (Some(value), Some(SOURCE_USER), Some(apply_method)),
            "case {case:?}: expected what the caller wrote for {parameter}, got {row:?}"
        );
        let published = published_by_the_engine_default_door(
            &router,
            case,
            Level::Cluster,
            family,
            parameter,
        )
        .await;
        assert_eq!(
            published_record(&row),
            published,
            "case {case:?}: {parameter} must carry its published record, got {row:?}"
        );
    }
}

/// The record Amazon RDS publishes for the PostgreSQL planner's page-cost setting, pinned
/// column by column.
///
/// Source: the engine's own settings catalog as Amazon RDS publishes it for the PostgreSQL
/// families — a dynamic, modifiable float whose domain is the floating-point range, and the
/// catalog sentence as written, its trailing period included. The columns of ONE named
/// parameter, so a projection that drops a column or re-words the sentence fails here rather
/// than only on the table above, which compares two doors with each other.
const PUBLISHED_FLOAT_RECORD: (&str, &str, &str, &str, &str) = (
    "Sets the planners estimate of the cost of a nonsequentially fetched disk page.",
    "dynamic",
    "float",
    "0-1.79769e+308",
    "true",
);

/// That record is what a group of either level reports for the parameter, on the row the
/// caller set and on the engine's own default row alike.
#[tokio::test]
async fn the_published_record_of_a_float_parameter_is_the_catalog_text() {
    let router = emulator();
    let parameter = "random_page_cost";
    let expected = (
        Some(PUBLISHED_FLOAT_RECORD.0.to_string()),
        Some(PUBLISHED_FLOAT_RECORD.1.to_string()),
        Some(PUBLISHED_FLOAT_RECORD.2.to_string()),
        Some(PUBLISHED_FLOAT_RECORD.3.to_string()),
        Some(PUBLISHED_FLOAT_RECORD.4.to_string()),
    );

    // Several families of the engine, at both levels: the record belongs to the parameter.
    let rows: [(&str, Level, &str); 3] = [
        ("an RDS for PostgreSQL family", Level::Instance, "postgres16"),
        ("another release of the same engine", Level::Instance, "postgres14"),
        (
            "an Aurora PostgreSQL cluster group of the same engine",
            Level::Cluster,
            "aurora-postgresql16",
        ),
    ];

    for (index, (case, level, family)) in rows.into_iter().enumerate() {
        let group = create_group(
            &router,
            case,
            level,
            &format!("emu-float-record-{index}"),
            family,
        )
        .await;
        expect_ok(
            case,
            modify(&router, level, &group, parameter, "2.5", None).await,
        );
        let reported = reported_rows(&router, case, level, &group, Some(SOURCE_USER)).await;
        let row = row_for(&reported, parameter)
            .unwrap_or_else(|| panic!("case {case:?}: expected {parameter}, got {reported:?}"));
        assert_eq!(
            published_record(&row),
            expected,
            "case {case:?}: expected the published record of {parameter}, got {row:?}"
        );
        if level == Level::Instance {
            // The same record at the FAMILY's own door, where the row is the engine's
            // default rather than the caller's value.
            let published =
                published_by_the_engine_default_door(&router, case, level, family, parameter)
                    .await;
            assert_eq!(
                published, expected,
                "case {case:?}: the engine-default door publishes the one record"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1: which names a group may carry at all
// ---------------------------------------------------------------------------

/// One row of the membership table: the case, the level, the family, the parameter the request
/// names, a value for it, and whether the group may carry that name at all.
type MembershipRow = (
    &'static str,
    Level,
    &'static str,
    &'static str,
    &'static str,
    bool,
);

/// The wording this front door refuses a parameter the engine has no setting for with.
fn not_found(parameter: &str) -> String {
    format!("Could not find parameter with name: {parameter}")
}

/// A group carries its family's engine's parameters and no others: a name the engine has no
/// setting for is refused on the modify and on the reset, and nothing is written; a name the
/// engine HAS is carried whether or not a catalog here publishes its record.
#[tokio::test]
async fn a_group_carries_only_the_parameters_its_familys_engine_has() {
    let router = emulator();

    let rows: [MembershipRow; 14] = [
        // A name no PostgreSQL release has a setting for: no parameter of the group, at
        // either level.
        ("a name the PostgreSQL engine has no setting for, cluster-level", Level::Cluster, "aurora-postgresql16", "no_such_postgres_setting", "1", false),
        ("a name the PostgreSQL engine has no setting for, instance-level", Level::Instance, "postgres16", "not_a_postgres_guc", "1", false),
        // The same for the MySQL-compatible engines, including the fork of that server.
        ("a name the MySQL engine has no setting for, cluster-level", Level::Cluster, "aurora-mysql8.0", "no_such_mysql_variable", "1", false),
        ("a name the MySQL engine has no setting for, instance-level", Level::Instance, "mysql8.0", "not_a_mysql_variable", "1", false),
        ("a name the MariaDB fork has no setting for", Level::Instance, "mariadb10.11", "not_a_mariadb_variable", "1", false),
        // A setting the engine HAS that no catalog here publishes a record for: carried,
        // because the published catalogs are samples of AWS's lists and the engine's
        // namespace is what says which parameters exist.
        ("an unpublished PostgreSQL setting, cluster-level", Level::Cluster, "aurora-postgresql16", "wal_buffers", "4096", true),
        ("an unpublished PostgreSQL setting, instance-level", Level::Instance, "postgres16", "effective_cache_size", "524288", true),
        ("an unpublished MySQL variable, cluster-level", Level::Cluster, "aurora-mysql8.0", "innodb_io_capacity", "2000", true),
        ("an unpublished MySQL variable, instance-level", Level::Instance, "mysql8.0", "innodb_old_blocks_time", "1000", true),
        ("an unpublished MariaDB variable", Level::Instance, "mariadb10.11", "aria_pagecache_buffer_size", "134217728", true),
        // The dotted form: PostgreSQL reads a name with a prefix as an extension's own
        // customized option, so the engine has a setting by that name whichever extension
        // defines it.
        ("an extension's customized option, cluster-level", Level::Cluster, "aurora-postgresql16", "apg_plan_mgmt.capture_plan_baselines", "automatic", true),
        ("an extension's customized option, instance-level", Level::Instance, "postgres16", "pg_stat_statements.max", "5000", true),
        // A family whose engine's namespace this front door does not carry makes no claim
        // either way, so any name is carried.
        ("a family of an engine with no carried namespace, cluster-level", Level::Cluster, "db2-se", "a_name_nothing_publishes", "1", true),
        ("a family of an engine with no carried namespace, instance-level", Level::Instance, "oracle-ee-19", "a_name_nothing_publishes", "1", true),
    ];

    for (index, (case, level, family, parameter, value, carried)) in rows.into_iter().enumerate() {
        let group = create_group(
            &router,
            case,
            level,
            &format!("emu-membership-{index}"),
            family,
        )
        .await;
        let answered = modify(&router, level, &group, parameter, value, None).await;

        if carried {
            expect_ok(case, answered);
            let rows = reported_rows(&router, case, level, &group, Some(SOURCE_USER)).await;
            let row = row_for(&rows, parameter).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the group must report the parameter it was given, got {:?}",
                    rows.iter().map(|row| &row.name).collect::<Vec<_>>()
                )
            });
            assert_eq!(
                row.value.as_deref(),
                Some(value),
                "case {case:?}: expected the value the caller set, got {row:?}"
            );
            continue;
        }

        let refused = expect_error(case, answered, 400, INVALID_PARAMETER_VALUE);
        let message = text(&refused, "Message");
        assert_eq!(
            message,
            not_found(parameter),
            "case {case:?}: expected the published refusal wording, got {message:?}"
        );

        // The reset that would put it back to its default is refused by the one resolution:
        // there is no such parameter of the group to reset either.
        let refused = expect_error(
            case,
            reset(&router, level, &group, parameter).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message,
            not_found(parameter),
            "case {case:?}: expected the published refusal wording from the reset, got \
             {message:?}"
        );

        // NEITHER read reports it: the narrowed one the caller confirms its change with, and
        // the whole report.
        for source in [None, Some(SOURCE_USER)] {
            let case = format!("{case} (Source {source:?})");
            let rows = reported_rows(&router, &case, level, &group, source).await;
            assert!(
                row_for(&rows, parameter).is_none(),
                "case {case:?}: a refused parameter is no member of the group, got {:?}",
                rows.iter().map(|row| &row.name).collect::<Vec<_>>()
            );
        }
    }
}

/// The engines whose settings namespace this front door carries, each with families of it at
/// both levels: the engines whose published RECORDS are a sample of AWS's list, so their
/// namespace is what answers membership.
const CARRIED_NAMESPACES: [(&str, &[&str]); 5] = [
    ("postgres", &["postgres14", "postgres16"]),
    ("aurora-postgresql", &["aurora-postgresql15", "aurora-postgresql16"]),
    ("mysql", &["mysql5.7", "mysql8.0"]),
    ("aurora-mysql", &["aurora-mysql5.7", "aurora-mysql8.0"]),
    ("mariadb", &["mariadb10.6", "mariadb10.11"]),
];

/// The two vocabularies cannot disagree: every parameter a family PUBLISHES a record for is a
/// setting its engine has.
///
/// The published catalogs decide a parameter's record and the namespace decides membership, so
/// a name in a catalog that the namespace does not carry would be a parameter this front door
/// describes and would refuse a write to. Driven by the catalogs THEMSELVES — every entry of
/// every published family of each carried engine, at both levels — rather than by named rows,
/// so a catalog entry added later is covered without touching this test.
#[tokio::test]
async fn every_published_parameter_is_a_setting_its_engine_has() {
    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for (engine, families) in CARRIED_NAMESPACES {
        for family in families {
            let published = published_db_parameters(family)
                .unwrap_or_default()
                .iter()
                .chain(published_cluster_parameters(family).unwrap_or_default().iter());
            for spec in published {
                checked += 1;
                if engine_has_no_setting_named(engine, spec.name) {
                    missing.push(format!("{engine} {family} {}", spec.name));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "case {:?}: an engine's settings namespace must carry every parameter a family of it \
         publishes a record for, and these are not carried: {missing:?}",
        "the derived table"
    );
    assert!(
        checked > 100,
        "case {:?}: the carried families publish catalogs, so the walk must reach their \
         entries, got {checked}",
        "the derived table"
    );
}
