// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `time_zone` parameter the MySQL-compatible engines publish.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, so the assertions read the
//! XML the emulator itself answered with.
//!
//! The rule under test: the server setting a client changes the database's TIME ZONE through
//! is a parameter of the MySQL-compatible engines' published catalogue, and its `ApplyType`
//! is `dynamic` — which is what makes a change to it take effect in the sessions opened
//! after it without a reboot, as opposed to a `static` parameter, which waits for one. So
//! every DB parameter group family of those engines reports it, with the value such an
//! instance runs in by default, and a group of such a family takes a value for it and
//! reports the value back as the caller's own.
//!
//! The table below spans what the rule generalises over: the families of the three
//! MySQL-compatible engines (Amazon RDS for MySQL in two release lines, including the one
//! line this parameter was reported missing from, Amazon RDS for MariaDB, and Aurora MySQL),
//! and both reports that publish a parameter's record — the engine's own defaults and a DB
//! parameter group's.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    APPLY_METHOD_IMMEDIATE, APPLY_TYPE_DYNAMIC, PARAMETER_SOURCE_ENGINE_DEFAULT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The parameter under test, and the time zone a group below sets it to: a zone name of the
/// engine's own time-zone table, which is what the parameter admits.
const PARAMETER: &str = "time_zone";
const ANOTHER_ZONE: &str = "Europe/Berlin";

/// The DB parameter group families the rule is asserted over: the MySQL-compatible engines
/// this front door serves, in more than one release line each where it has them.
const FAMILIES: [&str; 5] = [
    "mysql8.4",
    "mysql8.0",
    "mysql5.7",
    "mariadb10.11",
    "aurora-mysql8.0",
];

/// The one `<Parameter>` a report carries for [`PARAMETER`].
fn the_parameter(case: &str, body: &str) -> String {
    let found: Vec<String> = subtrees(body, "Parameter")
        .into_iter()
        .filter(|entry| optional_text(entry, "ParameterName").as_deref() == Some(PARAMETER))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {PARAMETER} entry, got {found:?} in {body}"
    );
    found.into_iter().next().expect("checked just above")
}

/// `DescribeEngineDefaultParameters` for one family, narrowed to the parameter under test.
async fn engine_default(router: &MultiServiceRouter, family: &str) -> String {
    expect_ok(
        family,
        call(
            router,
            "DescribeEngineDefaultParameters",
            &params![
                "DBParameterGroupFamily" => family,
                "Filters.Filter.1.Name" => "parameter-name",
                "Filters.Filter.1.Values.Value.1" => PARAMETER,
            ],
        )
        .await,
    )
}

#[tokio::test]
async fn every_mysql_family_publishes_a_dynamic_time_zone_parameter() {
    let router = emulator();
    for family in FAMILIES {
        // The engine's own defaults carry the parameter, dynamic, modifiable and with the
        // value such an instance runs in until a group sets another.
        let reported = engine_default(&router, family).await;
        let parameter = the_parameter(family, &reported);
        let apply_type = optional_text(&parameter, "ApplyType");
        assert_eq!(
            apply_type.as_deref(),
            Some(APPLY_TYPE_DYNAMIC),
            "case {family:?}: a time-zone change takes effect in new sessions without a \
             reboot, so expected ApplyType {APPLY_TYPE_DYNAMIC:?}, got {apply_type:?}"
        );
        assert_eq!(
            optional_text(&parameter, "Source").as_deref(),
            Some(PARAMETER_SOURCE_ENGINE_DEFAULT),
            "case {family:?}: the engine's own defaults are sourced \
             {PARAMETER_SOURCE_ENGINE_DEFAULT:?}, got {parameter}"
        );
        assert_eq!(
            optional_text(&parameter, "IsModifiable").as_deref(),
            Some("true"),
            "case {family:?}: a client sets the time zone through this parameter, so it is \
             modifiable, got {parameter}"
        );
        let value = optional_text(&parameter, "ParameterValue");
        assert!(
            value.as_ref().is_some_and(|value| !value.is_empty()),
            "case {family:?}: the parameter reports the zone the engine runs in by default, \
             got {value:?}"
        );

        // And a GROUP of that family publishes the same record — and takes a value for it,
        // which it then reports as the caller's own.
        let group = format!("emu-tz-{}", family.replace(['.', '-'], ""));
        expect_ok(
            family,
            call(
                &router,
                "CreateDBParameterGroup",
                &params![
                    "DBParameterGroupName" => &group,
                    "DBParameterGroupFamily" => family,
                    "Description" => "the time-zone suite",
                ],
            )
            .await,
        );
        let in_group = expect_ok(
            family,
            call(
                &router,
                "DescribeDBParameters",
                &params![
                    "DBParameterGroupName" => &group,
                    "Filters.Filter.1.Name" => "parameter-name",
                    "Filters.Filter.1.Values.Value.1" => PARAMETER,
                ],
            )
            .await,
        );
        let parameter = the_parameter(family, &in_group);
        assert_eq!(
            optional_text(&parameter, "ApplyType").as_deref(),
            Some(APPLY_TYPE_DYNAMIC),
            "case {family:?}: a group reports the engine's own apply type, got {parameter}"
        );
        expect_ok(
            family,
            call(
                &router,
                "ModifyDBParameterGroup",
                &params![
                    "DBParameterGroupName" => &group,
                    "Parameters.Parameter.1.ParameterName" => PARAMETER,
                    "Parameters.Parameter.1.ParameterValue" => ANOTHER_ZONE,
                    "Parameters.Parameter.1.ApplyMethod" => APPLY_METHOD_IMMEDIATE,
                ],
            )
            .await,
        );
        let read_back = expect_ok(
            family,
            call(
                &router,
                "DescribeDBParameters",
                &params![
                    "DBParameterGroupName" => &group,
                    "Filters.Filter.1.Name" => "parameter-name",
                    "Filters.Filter.1.Values.Value.1" => PARAMETER,
                ],
            )
            .await,
        );
        let parameter = the_parameter(family, &read_back);
        assert_eq!(
            text(&parameter, "ParameterValue"),
            ANOTHER_ZONE,
            "case {family:?}: the group reports the zone the caller set, got {parameter}"
        );
        assert_eq!(
            optional_text(&parameter, "ApplyType").as_deref(),
            Some(APPLY_TYPE_DYNAMIC),
            "case {family:?}: the set value keeps the parameter's own apply type, got \
             {parameter}"
        );
    }
}
