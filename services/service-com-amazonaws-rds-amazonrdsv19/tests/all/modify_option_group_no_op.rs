// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a `ModifyOptionGroup` that asks for NO option change.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on.
//!
//! The rule under test: what `ModifyOptionGroup` changes about a group is exactly the
//! options `OptionsToInclude` adds or updates and the ones `OptionsToRemove` drops, so a
//! request naming an option in neither list asks for no change at all and is refused with
//! HTTP 400 and the Query API's `InvalidParameterValue` — never answered 200 with the group
//! a client would then read as modified.
//!
//! The table below spans what the rule generalises over: the engine the group was created
//! for (one that publishes options, one that publishes options with settings, and one that
//! publishes none at all), whether the group already holds an option, and whether the
//! request carries `ApplyImmediately` — none of which changes the answer. A positive
//! control in each row shows that a request naming ONE option is still served, so the
//! refusal is of the empty request and not of the operation.

use service_com_amazonaws_rds_amazonrdsv19::state::option_catalog::published_option_names;
use service_com_amazonaws_rds_amazonrdsv19::state::AT_LEAST_ONE_OPTION_CHANGE_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};
use crate::option_group_options::{create_group, modify, reported_option_names};

/// One row of the no-op table: the engine the group is created for, and the forms of
/// "nothing to change" the request is written in.
struct NoOpCase {
    case: &'static str,
    /// The option group this row creates.
    group: &'static str,
    /// `EngineName` of that group, which decides which options it could hold at all.
    engine: &'static str,
}

/// The engines this rule is driven over: MariaDB (which publishes an option carrying
/// settings), SQL Server Enterprise (which publishes a PERMANENT option), and PostgreSQL,
/// which publishes no option at all — the rule is about the request, so it holds for a
/// group that could hold nothing just as for one that could hold something.
const NO_OP_CASES: &[NoOpCase] = &[
    NoOpCase {
        case: "a MariaDB group",
        group: "emu-noop-mariadb",
        engine: "mariadb",
    },
    NoOpCase {
        case: "a SQL Server Enterprise group",
        group: "emu-noop-sqlserver",
        engine: "sqlserver-ee",
    },
    NoOpCase {
        case: "a PostgreSQL group, whose engine publishes no option",
        group: "emu-noop-postgres",
        engine: "postgres",
    },
];

/// The ways a request can carry no option at all: neither list named, and the same request
/// carrying `ApplyImmediately` either way — the member chooses WHEN a change reaches the
/// instances the group is attached to, so it cannot make a request that changes nothing
/// into one that changes something.
const EMPTY_FORMS: &[(&str, Option<bool>)] = &[
    ("neither list named", None),
    ("neither list named, applied immediately", Some(true)),
    ("neither list named, applied in the window", Some(false)),
];

/// `ModifyOptionGroup` naming no option, in one of the [`EMPTY_FORMS`].
async fn modify_nothing(
    router: &MultiServiceRouter,
    group: &str,
    apply_immediately: Option<bool>,
) -> (u16, String) {
    let mut params: Vec<(String, String)> =
        vec![("OptionGroupName".to_string(), group.to_string())];
    if let Some(apply) = apply_immediately {
        params.push(("ApplyImmediately".to_string(), apply.to_string()));
    }
    call(router, "ModifyOptionGroup", &params).await
}

/// The option names `DescribeOptionGroups` reports for `group`.
async fn held_options(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeOptionGroups",
            &[("OptionGroupName".to_string(), group.to_string())],
        )
        .await,
    );
    reported_option_names(&body)
}

#[tokio::test]
async fn a_modify_option_group_naming_no_option_is_refused() {
    let router = emulator();
    for row in NO_OP_CASES {
        create_group(&router, row.case, row.group, row.engine).await;
        for (form, apply_immediately) in EMPTY_FORMS {
            let case = format!("{}, {form}", row.case);
            let body = expect_error(
                &case,
                modify_nothing(&router, row.group, *apply_immediately).await,
                400,
                "InvalidParameterValue",
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, AT_LEAST_ONE_OPTION_CHANGE_MESSAGE,
                "case {case:?}: expected the sentence a modification asking for no option \
                 change is refused with, got {message:?}"
            );
            // The refusal left the group as it was: it still holds no option.
            let held = held_options(&router, &case, row.group).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected a refused request to change nothing, got {held:?}"
            );
        }

        // The positive control: naming ONE option the group's engine publishes is served,
        // and a no-op request against the group that now holds it is refused just the same.
        let Some(option) = published_option_names(row.engine).into_iter().next() else {
            continue;
        };
        let added = format!("{}, adding {option}", row.case);
        expect_ok(&added, modify(&router, row.group, &[(option, &[])], &[]).await);
        let held = held_options(&router, &added, row.group).await;
        assert_eq!(
            held,
            vec![option.to_string()],
            "case {added:?}: expected the group to hold the option that was added, got {held:?}"
        );
        for (form, apply_immediately) in EMPTY_FORMS {
            let case = format!("{}, {form}, with an option already held", row.case);
            expect_error(
                &case,
                modify_nothing(&router, row.group, *apply_immediately).await,
                400,
                "InvalidParameterValue",
            );
            let still_held = held_options(&router, &case, row.group).await;
            assert_eq!(
                still_held,
                vec![option.to_string()],
                "case {case:?}: expected a refused request to leave the held option alone, got \
                 {still_held:?}"
            );
        }
    }
}
