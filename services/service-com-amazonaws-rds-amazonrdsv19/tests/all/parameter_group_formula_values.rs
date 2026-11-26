// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a parameter value that is a FORMULA rather than a number.
//!
//! RULE: Amazon RDS lets the value of a numeric parameter be an expression over the DB
//! instance class's hardware — the form this front door's own parameter catalogs publish
//! several defaults as (`{DBInstanceClassMemory*3/4}` for the InnoDB buffer pool) — and the
//! service evaluates it for the class the resource runs on. Such a value is therefore NOT a
//! number, so the parameter's published `AllowedValues` range says nothing about it: a modify
//! that sets a formula on a numeric parameter is served, the group reports the formula back as
//! the value it was given, and the engine of a resource using that group runs the number the
//! expression comes to. Every value that IS a number is still held to the published range, a
//! parameter the engine does not let a caller modify still takes no value at all, and a
//! parameter whose published type is not numeric is still held to its own published domain.
//!
//! The rule is one rule at BOTH levels, so it is pinned on `ModifyDBParameterGroup` (a DB
//! parameter group) and on `ModifyDBClusterParameterGroup` (a DB cluster parameter group),
//! over tables that span what it generalises over: the formula forms the grammar has (a
//! quotient, a product with a quotient, a floor, a ceiling, a sum, a bare variable), the
//! parameter the formula is set on, and — on the refusing side — a literal below the published
//! minimum, a literal one past the published maximum, a brace expression that is no formula at
//! all, and a formula on a parameter whose published type is boolean.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    published_cluster_parameters, published_db_parameter, PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_parameter_groups::{
    create_group as create_cluster_group, modify as modify_cluster,
};
use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};

/// The error code Amazon RDS answers a parameter value its parameter does not admit with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The DB parameter group family the instance-level rows are written for: one release line of
/// the MySQL vocabulary, whose memory-formula defaults this service publishes.
const MYSQL_FAMILY: &str = "mysql8.0";

/// The DB CLUSTER parameter group family the cluster-level rows are written for.
const AURORA_MYSQL_FAMILY: &str = "aurora-mysql8.0";

/// The parameter whose published default IS a formula, and the one the issue cited.
const BUFFER_POOL: &str = "innodb_buffer_pool_size";

/// A second numeric parameter of the same family, whose published default is a formula too.
const MAX_CONNECTIONS: &str = "max_connections";

/// A parameter of the same family whose published `DataType` is boolean: its domain is the two
/// values a switch has, so no expression is a value of it.
const SLOW_QUERY_LOG: &str = "slow_query_log";

/// A numeric parameter of the CLUSTER-level catalog.
///
/// Source: the Aurora MySQL cluster parameter list this front door publishes
/// (`DescribeEngineDefaultClusterParameters`), which carries it as an integer parameter with a
/// published range.
const AURORA_READ_BUFFER: &str = "aurora_binlog_read_buffer_size";

/// The formula forms under test, each a value a caller may set.
///
/// Source: the "DB parameter formulas" grammar of the Amazon RDS User Guide — a braced
/// expression over formula variables and whole numbers, with `+ - * /` and the functions
/// `LEAST`, `GREATEST` and `SUM` — and the expressions this service's own catalogs publish as
/// defaults, which are values of that same grammar.
const FORMULAS: [(&str, &str); 6] = [
    ("the published default of the buffer pool", "{DBInstanceClassMemory*3/4}"),
    ("a plain quotient of the class memory", "{DBInstanceClassMemory/32768}"),
    (
        "a quotient under a ceiling",
        "LEAST({DBInstanceClassMemory/12582880},5000)",
    ),
    (
        "a quotient over a floor",
        "GREATEST({DBInstanceClassMemory/9531392},1024)",
    ),
    (
        "a sum of two formula terms",
        "SUM({DBInstanceClassMemory/9531392},{DBInstanceClassMemory/9531392})",
    ),
    ("the bare variable", "{DBInstanceClassMemory}"),
];

/// The value a group reports as its OWN (`Source` `user`) for `name`, or `None` where the
/// group carries no override for it.
async fn user_value(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> Option<String> {
    let reported = expect_ok(
        case,
        describe(
            router,
            group,
            &[("Source", PARAMETER_SOURCE_USER.to_string())],
        )
        .await,
    );
    subtrees(&reported, "Parameter")
        .into_iter()
        .find(|subtree| text(subtree, "ParameterName") == name)
        .map(|subtree| text(&subtree, "ParameterValue"))
}

/// The value a CLUSTER parameter group reports as its own for `name`.
async fn cluster_user_value(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> Option<String> {
    let reported = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusterParameters",
            &[
                ("DBClusterParameterGroupName", group),
                ("Source", PARAMETER_SOURCE_USER),
            ],
        )
        .await,
    );
    subtrees(&reported, "Parameter")
        .into_iter()
        .find(|subtree| text(subtree, "ParameterName") == name)
        .map(|subtree| text(&subtree, "ParameterValue"))
}

/// Rule: a formula is a value a numeric parameter of a DB parameter group takes, and the group
/// reports it back as the caller wrote it.
///
/// The rows span the forms the formula grammar has, on two parameters of the family whose own
/// published defaults are formulas — including forms neither the issue nor this catalog cited.
#[tokio::test]
async fn a_formula_is_a_value_a_numeric_db_parameter_takes() {
    let router = emulator();
    let group = create_group(&router, "formula values", "emu-formula-db", MYSQL_FAMILY).await;

    for name in [BUFFER_POOL, MAX_CONNECTIONS] {
        // The parameter really is one this family publishes a numeric domain for, so the rows
        // below are about a value the published range would otherwise refuse.
        let spec = published_db_parameter(MYSQL_FAMILY, name)
            .unwrap_or_else(|| panic!("case {name:?}: the family must publish the parameter"));
        assert_eq!(
            spec.data_type, "integer",
            "case {name:?}: expected a numeric parameter, got {:?}",
            spec.data_type
        );

        for (form, formula) in FORMULAS {
            let case = format!("{name}: {form}");
            expect_ok(
                &case,
                modify_in(&router, REGION, &group, &[(name, formula, None)]).await,
            );
            let stored = user_value(&router, &case, &group, name).await;
            assert_eq!(
                stored.as_deref(),
                Some(formula),
                "case {case:?}: expected the group to report the formula {formula:?} it was \
                 given, got {stored:?}"
            );
        }
    }
}

/// Rule, the other side: a value that is NOT a formula is still held to the published domain
/// of its parameter, and the refusal names the published range whole.
///
/// The rows span a literal below the published minimum (the issue's own case), a literal one
/// past the published maximum, a braced expression that is no formula of the grammar, and a
/// formula on a parameter whose published type is boolean — each refused with the sentence its
/// own cause carries.
#[tokio::test]
async fn a_value_that_is_no_formula_is_still_held_to_the_published_domain() {
    let router = emulator();
    let group = create_group(&router, "formula refusals", "emu-formula-refused", MYSQL_FAMILY)
        .await;
    let spec = published_db_parameter(MYSQL_FAMILY, BUFFER_POOL)
        .unwrap_or_else(|| panic!("the family must publish {BUFFER_POOL}"));
    let (low, high) = spec
        .range()
        .unwrap_or_else(|| panic!("{BUFFER_POOL} must publish a range"));
    let (low, high) = (low.trim(), high.trim());
    let below: u128 = low.parse::<u128>().expect("an integer lower bound") - 1;
    let past: u128 = high.parse::<u128>().expect("an integer upper bound") + 1;

    // (case, the parameter, the value, the sentence the refusal must carry)
    let rows: [(&str, &str, String, String); 4] = [
        (
            "a literal one below the published minimum",
            BUFFER_POOL,
            below.to_string(),
            format!(
                "Value: {below} is outside of range: {}-{} for parameter: {BUFFER_POOL}",
                low, high
            ),
        ),
        (
            "a literal one past the published maximum",
            BUFFER_POOL,
            past.to_string(),
            format!(
                "Value: {past} is outside of range: {}-{} for parameter: {BUFFER_POOL}",
                low, high
            ),
        ),
        (
            "a braced expression that is no formula of the grammar",
            BUFFER_POOL,
            "{DBInstanceClassMemory/}".to_string(),
            format!(
                "Value: {{DBInstanceClassMemory/}} is outside of range: {}-{} for parameter: \
                 {BUFFER_POOL}",
                low, high
            ),
        ),
        (
            "a formula on a parameter whose published type is boolean",
            SLOW_QUERY_LOG,
            "{DBInstanceClassMemory}".to_string(),
            "Invalid boolean value: {DBInstanceClassMemory}".to_string(),
        ),
    ];

    for (case, name, value, sentence) in rows {
        let body = expect_error(
            case,
            modify_in(&router, REGION, &group, &[(name, &value, None)]).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, sentence,
            "case {case:?}: expected the sentence {sentence:?}, got {message:?}"
        );
        let stored = user_value(&router, case, &group, name).await;
        assert_eq!(
            stored, None,
            "case {case:?}: a refused value must leave the group carrying no override for \
             {name}, got {stored:?}"
        );
    }
}

/// Rule: it is ONE rule, so a DB CLUSTER parameter group takes a formula on a numeric
/// cluster-level parameter too, reports it back, and still refuses a literal outside that
/// parameter's published range.
#[tokio::test]
async fn the_formula_rule_holds_for_a_cluster_parameter_group() {
    let router = emulator();
    let group = create_cluster_group(
        &router,
        "cluster formula values",
        "emu-formula-cluster",
        AURORA_MYSQL_FAMILY,
    )
    .await;
    let spec = published_cluster_parameters(AURORA_MYSQL_FAMILY)
        .and_then(|catalog| catalog.iter().find(|spec| spec.name == AURORA_READ_BUFFER))
        .unwrap_or_else(|| panic!("the family must publish {AURORA_READ_BUFFER}"));
    let (low, high) = spec
        .range()
        .unwrap_or_else(|| panic!("{AURORA_READ_BUFFER} must publish a range"));
    let (low, high) = (low.trim(), high.trim());

    for (form, formula) in FORMULAS {
        let case = format!("{AURORA_READ_BUFFER}: {form}");
        expect_ok(
            &case,
            modify_cluster(&router, &group, AURORA_READ_BUFFER, formula, None).await,
        );
        let stored = cluster_user_value(&router, &case, &group, AURORA_READ_BUFFER).await;
        assert_eq!(
            stored.as_deref(),
            Some(formula),
            "case {case:?}: expected the group to report the formula {formula:?} it was given, \
             got {stored:?}"
        );
    }

    let past = high.parse::<u128>().expect("an integer upper bound") + 1;
    let case = format!("{AURORA_READ_BUFFER}: a literal one past the published maximum");
    let body = expect_error(
        &case,
        modify_cluster(
            &router,
            &group,
            AURORA_READ_BUFFER,
            &past.to_string(),
            None,
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    let sentence = format!(
        "Value: {past} is outside of range: {low}-{high} for parameter: {AURORA_READ_BUFFER}"
    );
    assert_eq!(
        message, sentence,
        "case {case:?}: expected the sentence {sentence:?}, got {message:?}"
    );
}
