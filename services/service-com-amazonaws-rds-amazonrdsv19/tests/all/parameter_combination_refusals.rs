// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals this front door classifies as a PARAMETER COMBINATION
//! rather than as a bad value.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answered with.
//!
//! The rule under test: where two members of one request cannot hold together — a pair whose
//! documentation states one member's window IN TERMS of the other's value, or a member whose
//! documentation makes it depend on another being supplied — the request is refused HTTP 400
//! `InvalidParameterCombination`, the Query API's published code for parameters that cannot
//! be used together, stating the relation the pair has to satisfy. A value outside the window
//! its OWN member documents keeps `InvalidParameterValue`, the code for a value a parameter
//! does not accept, so a client can tell the two classes apart. Either refusal is ATOMIC:
//! nothing of the request is applied.
//!
//! The tables below span what the rule generalises over: both of the relations this front
//! door states (the pool pair of a DB proxy target group, and the engine-name dependency of
//! the option-group report), each at its boundary and one step past it, the member pair in
//! both orders — stated together in one call and one member against the value already in
//! force — and the single-member rows of the same requests, which keep the value code.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};
use crate::db_proxy_target_groups::create_proxy;

/// The Query API code a value a member does not accept is refused under.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The Query API code members that cannot be used together are refused under.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// The `MaxConnectionsPercent` a target group starts with for every engine family but SQL
/// Server ("Default: ... 100 for all other engines"), and so the ceiling
/// `MaxIdleConnectionsPercent` is held to before any modification moves it.
const DEFAULT_MAX_CONNECTIONS_PERCENT: i32 = 100;

/// `ModifyDBProxyTargetGroup` on that one group, carrying the pool settings named.
async fn modify_pool(
    router: &MultiServiceRouter,
    proxy: &str,
    settings: &[(&str, String)],
) -> (u16, String) {
    let mut params: Vec<(String, String)> = vec![
        ("DBProxyName".to_string(), proxy.to_string()),
        ("TargetGroupName".to_string(), "default".to_string()),
    ];
    for (member, value) in settings {
        params.push((
            format!("ConnectionPoolConfig.{member}"),
            value.to_string(),
        ));
    }
    call(router, "ModifyDBProxyTargetGroup", &params).await
}

/// One pool setting of the group a body reports.
fn pool_setting(body: &str, member: &str) -> String {
    text(body, member)
}

/// One row of the pool table: the settings the request states, and the refusal it answers
/// (or no refusal at all, for a row at the boundary the pair admits).
struct PoolRow {
    case: &'static str,
    /// The `ConnectionPoolConfig` members the request states.
    settings: Vec<(&'static str, String)>,
    /// The Query API code the refusal carries, or `None` for a row that is SERVED.
    code: Option<&'static str>,
    /// The sentence the refusal states, where the row pins one.
    message: Option<String>,
}

/// One row of the option-group table: the parameters the request carries, and the refusal it
/// answers.
struct SelectionRow {
    case: &'static str,
    params: Vec<(&'static str, &'static str)>,
    code: &'static str,
    /// The sentence the refusal states, where the row pins one.
    message: Option<&'static str>,
}

#[tokio::test]
async fn the_pool_pair_that_cannot_hold_together_is_a_parameter_combination() {
    let router = emulator();
    let proxy = "emu-combination-pool";
    create_proxy(&router, proxy, "MYSQL").await;

    // The baseline every refusal below is judged against: a pool configuration stating every
    // field, accepted.
    let baseline: &[(&str, String)] = &[
        ("MaxConnectionsPercent", "73".to_string()),
        ("MaxIdleConnectionsPercent", "41".to_string()),
        ("ConnectionBorrowTimeout", "37".to_string()),
        (
            "InitQuery",
            "SET sql_mode='STRICT_ALL_TABLES'".to_string(),
        ),
        (
            "SessionPinningFilters.member.1",
            "EXCLUDE_VARIABLE_SETS".to_string(),
        ),
    ];
    let applied = expect_ok("the baseline pool configuration", modify_pool(&router, proxy, baseline).await);
    assert_eq!(
        (
            pool_setting(&applied, "MaxConnectionsPercent"),
            pool_setting(&applied, "MaxIdleConnectionsPercent"),
            pool_setting(&applied, "ConnectionBorrowTimeout"),
        ),
        ("73".to_string(), "41".to_string(), "37".to_string()),
        "the baseline must be applied as stated, got {applied}"
    );

    // `MaxIdleConnectionsPercent` "must be between 0 and the value of
    // MaxConnectionsPercent", and `MaxConnectionsPercent` "must be between 1 and 100": the
    // rows span the pair above the ceiling in force, the pair stated TOGETHER in one call,
    // the ceiling itself (served), and each member's own window at and past its bound.
    let rows: Vec<PoolRow> = vec![
        PoolRow {
            case: "an idle percentage above the pool size in force",
            settings: vec![("MaxIdleConnectionsPercent", "80".to_string())],
            code: Some(INVALID_PARAMETER_COMBINATION),
            message: Some(
                "MaxConnectionsPercent 73 must be greater than MaxIdleConnectionsPercent 80"
                    .to_string(),
            ),
        },
        PoolRow {
            case: "every other pool field changed beside it, which is refused all the same",
            settings: vec![
                ("MaxIdleConnectionsPercent", "80".to_string()),
                ("ConnectionBorrowTimeout", "11".to_string()),
                ("InitQuery", "SET sql_mode=''".to_string()),
            ],
            code: Some(INVALID_PARAMETER_COMBINATION),
            message: Some(
                "MaxConnectionsPercent 73 must be greater than MaxIdleConnectionsPercent 80"
                    .to_string(),
            ),
        },
        PoolRow {
            case: "the pair stated together, with the idle percentage above the pool size",
            settings: vec![
                ("MaxConnectionsPercent", "20".to_string()),
                ("MaxIdleConnectionsPercent", "21".to_string()),
            ],
            code: Some(INVALID_PARAMETER_COMBINATION),
            message: Some(
                "MaxConnectionsPercent 20 must be greater than MaxIdleConnectionsPercent 21"
                    .to_string(),
            ),
        },
        PoolRow {
            case: "the highest idle percentage the pool size in force admits",
            settings: vec![("MaxIdleConnectionsPercent", "73".to_string())],
            code: None,
            message: None,
        },
        PoolRow {
            case: "one past it",
            settings: vec![("MaxIdleConnectionsPercent", "74".to_string())],
            code: Some(INVALID_PARAMETER_COMBINATION),
            message: Some(
                "MaxConnectionsPercent 73 must be greater than MaxIdleConnectionsPercent 74"
                    .to_string(),
            ),
        },
        PoolRow {
            // One member's own window is its alone, so a value outside it keeps the code for
            // a value a parameter does not accept.
            case: "a pool size one past the highest it publishes",
            settings: vec![(
                "MaxConnectionsPercent",
                (DEFAULT_MAX_CONNECTIONS_PERCENT + 1).to_string(),
            )],
            code: Some(INVALID_PARAMETER_VALUE),
            message: None,
        },
        PoolRow {
            case: "a pool size one below the lowest it publishes",
            settings: vec![("MaxConnectionsPercent", "0".to_string())],
            code: Some(INVALID_PARAMETER_VALUE),
            message: None,
        },
        PoolRow {
            case: "an idle percentage below its own floor",
            settings: vec![("MaxIdleConnectionsPercent", "-1".to_string())],
            code: Some(INVALID_PARAMETER_VALUE),
            message: None,
        },
        PoolRow {
            case: "a borrow timeout one past the highest it publishes",
            settings: vec![("ConnectionBorrowTimeout", "301".to_string())],
            code: Some(INVALID_PARAMETER_VALUE),
            message: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let Some(code) = row.code else {
            // The served row: the boundary itself is accepted, and it moves the group.
            let served = expect_ok(case, modify_pool(&router, proxy, &row.settings).await);
            assert_eq!(
                pool_setting(&served, "MaxIdleConnectionsPercent"),
                row.settings[0].1,
                "case {case:?}: expected the stated idle percentage applied, got {served}"
            );
            // Put the baseline back, so the rows after this one are judged against it.
            expect_ok(
                "restore the baseline",
                modify_pool(&router, proxy, baseline).await,
            );
            continue;
        };
        let refused = expect_error(
            case,
            modify_pool(&router, proxy, &row.settings).await,
            400,
            code,
        );
        if let Some(expected) = row.message {
            assert_eq!(
                text(&refused, "Message"),
                expected,
                "case {case:?}: expected {expected:?}, got {refused}"
            );
        }
        // The refusal is ATOMIC: every pool field still carries the value the baseline left,
        // including the ones the refused request also stated.
        let read_back = expect_ok(
            case,
            call(
                &router,
                "DescribeDBProxyTargetGroups",
                &[("DBProxyName", proxy)],
            )
            .await,
        );
        assert_eq!(
            (
                pool_setting(&read_back, "MaxConnectionsPercent"),
                pool_setting(&read_back, "MaxIdleConnectionsPercent"),
                pool_setting(&read_back, "ConnectionBorrowTimeout"),
                pool_setting(&read_back, "InitQuery"),
            ),
            (
                "73".to_string(),
                "41".to_string(),
                "37".to_string(),
                "SET sql_mode='STRICT_ALL_TABLES'".to_string(),
            ),
            "case {case:?}: a refused modification must leave every pool field as it was, \
             got {read_back}"
        );
    }
}

#[tokio::test]
async fn the_option_group_report_states_its_member_dependencies_as_a_combination() {
    let router = emulator();
    expect_ok(
        "seed a group",
        call(
            &router,
            "CreateOptionGroup",
            &[
                ("OptionGroupName", "emu-combination-og"),
                ("EngineName", "mysql"),
                ("MajorEngineVersion", "8.0"),
                ("OptionGroupDescription", "a group under test"),
            ],
        )
        .await,
    );

    // (case, the parameters, the expected code, the expected message)
    //
    // `MajorEngineVersion` — "if specified, then EngineName must also be specified" — and
    // `OptionGroupName` — "can't be supplied together with EngineName or
    // MajorEngineVersion" — are the two dependencies this report publishes; both are members
    // that cannot be used together, in both orders.
    let rows: Vec<SelectionRow> = vec![
        SelectionRow {
            case: "a major version with no engine name",
            params: vec![("MajorEngineVersion", "8.0")],
            code: INVALID_PARAMETER_COMBINATION,
            message: Some(
                "If MajorEngineVersion is specified, then EngineName must also be specified.",
            ),
        },
        SelectionRow {
            case: "another engine's major version with no engine name",
            params: vec![("MajorEngineVersion", "16")],
            code: INVALID_PARAMETER_COMBINATION,
            message: Some(
                "If MajorEngineVersion is specified, then EngineName must also be specified.",
            ),
        },
        SelectionRow {
            case: "a group name together with an engine name",
            params: vec![
                ("OptionGroupName", "emu-combination-og"),
                ("EngineName", "mysql"),
            ],
            code: INVALID_PARAMETER_COMBINATION,
            message: Some(
                "OptionGroupName can't be supplied together with EngineName or \
                 MajorEngineVersion.",
            ),
        },
        SelectionRow {
            case: "a group name together with a major version",
            params: vec![
                ("OptionGroupName", "emu-combination-og"),
                ("MajorEngineVersion", "8.0"),
            ],
            code: INVALID_PARAMETER_COMBINATION,
            message: Some(
                "OptionGroupName can't be supplied together with EngineName or \
                 MajorEngineVersion.",
            ),
        },
        SelectionRow {
            // A parameter whose value this report serves none of is the OTHER class: the
            // value is the problem, not a pair of members.
            case: "a filter, which the member says is unsupported",
            params: vec![
                ("Filters.Filter.1.Name", "engine"),
                ("Filters.Filter.1.Values.Value.1", "mysql"),
            ],
            code: INVALID_PARAMETER_VALUE,
            message: None,
        },
        SelectionRow {
            case: "an engine name outside the published list",
            params: vec![("EngineName", "not-an-engine")],
            code: INVALID_PARAMETER_VALUE,
            message: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let refused = expect_error(
            case,
            call(&router, "DescribeOptionGroups", &row.params).await,
            400,
            row.code,
        );
        if let Some(expected) = row.message {
            assert_eq!(
                text(&refused, "Message"),
                expected,
                "case {case:?}: expected {expected:?}, got {refused}"
            );
        }
    }

    // The CONTROL: the same members, paired as their documentation requires, still select the
    // group.
    let served = expect_ok(
        "the pair the documentation requires",
        call(
            &router,
            "DescribeOptionGroups",
            &[("EngineName", "mysql"), ("MajorEngineVersion", "8.0")],
        )
        .await,
    );
    assert!(
        texts(&served, "OptionGroupName").contains(&"emu-combination-og".to_string()),
        "expected the seeded group reported, got {served}"
    );
}
