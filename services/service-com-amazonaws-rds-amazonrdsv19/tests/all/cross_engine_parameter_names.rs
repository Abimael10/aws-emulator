// SPDX-License-Identifier: CC0-1.0

//! A parameter belongs to an ENGINE, so a parameter group carries no parameter of another
//! engine.
//!
//! The rule under test: a `ParameterName` this front door publishes for an engine it serves
//! other than the engine of the group's family, and for no level of that family's own engine,
//! names no parameter of the group — so the request is refused with HTTP 400
//! `InvalidParameterValue` and the published not-found wording, and nothing is written. It
//! holds at both LEVELS (a DB cluster parameter group and a DB parameter group) and for both
//! operations that name a parameter of a group (the modify that would SET it and the reset
//! that would clear it): there is no such parameter to set, and none to put back to its
//! default either.
//!
//! What the rule does NOT refuse, and the tables below span it: a parameter of the family's
//! own engine at either level, a parameter two engines both publish, a name this front door
//! publishes for no engine at all (the DB- and cluster-parameter catalogs are samples of
//! AWS's published lists wherever the open-source engine's own system variables are the
//! vocabulary, so a name outside one is no evidence that the engine lacks it), and any name
//! at all on a family of an engine this door publishes no vocabulary for.
//!
//! Every parameter named below comes from this front door's own published catalogs — the ones
//! `DescribeEngineDefaultParameters` and `DescribeEngineDefaultClusterParameters` report —
//! and every call goes over the wire through the harness in `db_instances.rs`.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_parameter_groups::create_group as create_cluster_group;
use crate::cluster_parameter_groups::modify as modify_cluster_group;
use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};
use crate::db_parameters::{create_group as create_db_group, modify_in as modify_db_group};

/// The Query API code Amazon RDS answers a value its own member does not admit with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The region the DB-parameter-group calls below are signed for; the same one
/// [`crate::db_instances::REGION`] names, passed through because the instance-level modify
/// helper takes a region.
const REGION: &str = crate::db_instances::REGION;

/// One row: the case, the group's family, the parameter the request names, the value it sets
/// and whether the group may carry that name at all.
type Row = (&'static str, &'static str, &'static str, &'static str, bool);

/// The refusal wording this front door answers a parameter no family of the group's engine
/// publishes with, as the shared catalog states it.
fn not_found(parameter: &str) -> String {
    format!("Could not find parameter with name: {parameter}")
}

/// The user-set parameter names a DB CLUSTER parameter group reports.
async fn cluster_settings(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let reported = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusterParameters",
            &[("DBClusterParameterGroupName", group), ("Source", "user")],
        )
        .await,
    );
    texts(&reported, "ParameterName")
}

/// The user-set parameter names a DB (instance) parameter group reports.
async fn db_settings(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let reported = expect_ok(
        case,
        call(
            router,
            "DescribeDBParameters",
            &[("DBParameterGroupName", group), ("Source", "user")],
        )
        .await,
    );
    texts(&reported, "ParameterName")
}

#[tokio::test]
async fn a_cluster_group_refuses_another_engines_parameter() {
    let router = emulator();

    let rows: [Row; 11] = [
        // Another engine's parameter, across the vocabularies this door publishes: the two
        // service-managed ones (Amazon Neptune's two levels, Amazon DocumentDB's) and the
        // open-source engines' own system variables, in both directions.
        ("a Neptune cluster parameter on a MySQL-compatible family", "aurora-mysql8.0", "neptune_enable_audit_log", "1", false),
        ("a Neptune DB parameter on a PostgreSQL-compatible family", "aurora-postgresql16", "neptune_query_timeout", "60000", false),
        ("an Amazon DocumentDB parameter on a MySQL-compatible family", "aurora-mysql8.0", "ttl_monitor", "enabled", false),
        ("a PostgreSQL parameter on a MySQL-compatible family", "aurora-mysql8.0", "autovacuum", "1", false),
        ("a MySQL parameter on a PostgreSQL-compatible family", "aurora-postgresql16", "autocommit", "0", false),
        ("a MySQL parameter on an Amazon DocumentDB family", "docdb5.0", "binlog_format", "ROW", false),
        ("an Oracle parameter on a Neptune family", "neptune1.3", "open_cursors", "300", false),
        // The family's own engine publishes it, at either level: carried.
        ("the family's own cluster parameter", "aurora-mysql8.0", "binlog_format", "ROW", true),
        ("the family's engine's own DB parameter", "aurora-mysql8.0", "max_connections", "1000", true),
        // A parameter two engines both publish is each engine's own.
        ("a parameter both engines publish", "aurora-postgresql16", "max_connections", "1000", true),
        // A family of an engine this door publishes no vocabulary for: no claim either way.
        ("another engine's parameter on a family with no published vocabulary", "db2-se", "neptune_enable_audit_log", "1", true),
    ];

    for (index, (case, family, parameter, value, carried)) in rows.into_iter().enumerate() {
        let group =
            create_cluster_group(&router, case, &format!("cross-engine-cpg-{index}"), family).await;
        let answered = modify_cluster_group(&router, &group, parameter, value, None).await;

        if carried {
            expect_ok(case, answered);
            let reported = cluster_settings(&router, case, &group).await;
            assert!(
                reported.contains(&parameter.to_string()),
                "case {case:?}: the group must report the parameter it was given, got \
                 {reported:?}"
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

        // The reset that would clear it is refused by the same resolution: a parameter of
        // another engine is none of this group's to put back to its default either.
        let reset = expect_error(
            case,
            call(
                &router,
                "ResetDBClusterParameterGroup",
                &[
                    ("DBClusterParameterGroupName", group.as_str()),
                    ("Parameters.Parameter.1.ParameterName", parameter),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let reset_message = text(&reset, "Message");
        assert_eq!(
            reset_message,
            not_found(parameter),
            "case {case:?}: expected the published refusal wording from the reset, got \
             {reset_message:?}"
        );

        let reported = cluster_settings(&router, case, &group).await;
        assert!(
            !reported.contains(&parameter.to_string()),
            "case {case:?}: a refused parameter must not be in state, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_db_parameter_group_refuses_another_engines_parameter() {
    let router = emulator();

    let rows: [Row; 9] = [
        ("a Neptune cluster parameter on a MySQL family", "mysql8.0", "neptune_enable_audit_log", "1", false),
        ("a Neptune DB parameter on a PostgreSQL family", "postgres14", "neptune_query_timeout", "60000", false),
        ("an Amazon DocumentDB parameter on a MariaDB family", "mariadb10.6", "profiler", "enabled", false),
        ("a PostgreSQL parameter on a MySQL family", "mysql8.0", "autovacuum", "1", false),
        ("a MySQL parameter on a PostgreSQL family", "postgres14", "autocommit", "0", false),
        ("an Oracle parameter on a SQL Server family", "sqlserver-ex-15.0", "open_cursors", "300", false),
        // The family's own engine publishes it: carried.
        ("the family's own parameter", "mysql8.0", "autocommit", "0", true),
        // A name this front door publishes for no engine at all: the catalog is a sample of
        // the engine's own system variables, and this one is a MySQL variable past its head.
        ("a name no published vocabulary carries", "mysql8.0", "innodb_adaptive_hash_index", "1", true),
        ("another engine's parameter on a family with no published vocabulary", "db2-se", "neptune_query_timeout", "60000", true),
    ];

    for (index, (case, family, parameter, value, carried)) in rows.into_iter().enumerate() {
        let group =
            create_db_group(&router, case, &format!("cross-engine-dpg-{index}"), family).await;
        let answered =
            modify_db_group(&router, REGION, &group, &[(parameter, value, None)]).await;

        if carried {
            expect_ok(case, answered);
            let reported = db_settings(&router, case, &group).await;
            assert!(
                reported.contains(&parameter.to_string()),
                "case {case:?}: the group must report the parameter it was given, got \
                 {reported:?}"
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

        let reset = expect_error(
            case,
            call(
                &router,
                "ResetDBParameterGroup",
                &[
                    ("DBParameterGroupName", group.as_str()),
                    ("Parameters.Parameter.1.ParameterName", parameter),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let reset_message = text(&reset, "Message");
        assert_eq!(
            reset_message,
            not_found(parameter),
            "case {case:?}: expected the published refusal wording from the reset, got \
             {reset_message:?}"
        );

        let reported = db_settings(&router, case, &group).await;
        assert!(
            !reported.contains(&parameter.to_string()),
            "case {case:?}: a refused parameter must not be in state, got {reported:?}"
        );
    }
}
