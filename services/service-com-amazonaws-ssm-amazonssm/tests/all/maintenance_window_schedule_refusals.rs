// SPDX-License-Identifier: CC0-1.0

//! How a maintenance window's `Schedule` is refused when the service's cron parser will not
//! read it: by WHERE the parse failed and WHICH rule the expression broke, never by one
//! sentence for every malformed expression.
//!
//! "The schedule of the maintenance window in the form of a cron or rate expression"
//! (`CreateMaintenanceWindow$Schedule`) is read by the service's cron parser, and a body it
//! refuses is answered HTTP 400 `ValidationException` whose message is
//! `Invalid CRON expression: error at position <position>, details: <detail>.` — the fixed
//! text around the two values that come out of the parse of the SUBMITTED expression: the
//! position it stopped at and the sentence stating the rule the field broke. A caller
//! debugging a rejected schedule is told which part of its expression the service would not
//! take.
//!
//! Both operations that WRITE a window's schedule are held to the one rule, and a refused
//! write leaves nothing behind: no window is created and no stored schedule changes.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`error_code`], [`error_message`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service
//! router.
//!
//! The suites are tables: the rows differ along which field of the expression fails, the
//! kind of rule it breaks (a wildcard standing where it states nothing, a value past the
//! range its field admits, characters the grammar does not read) and how many fields the
//! body carries — rather than the single expression a recorded unit happened to submit.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The refusal's fixed text, as the service words it around the two values the parse yields.
fn invalid_cron_message(position: i64, detail: &str) -> String {
    format!("Invalid CRON expression: error at position {position}, details: {detail}")
}

/// The detail a field stating "no specific value" where that states nothing breaks: the `?`
/// wildcard stands for the day of the month or the day of the week and for nothing else.
const NO_SPECIFIC_VALUE: &str = "'?' can only be specified for Day-of-Month or Day-of-Week.";

/// The detail a body that runs out of fields breaks.
const UNEXPECTED_END: &str = "Unexpected end of expression.";

/// A create of a window named `name` with `schedule`, every other required member minimal.
async fn create(router: &MultiServiceRouter, name: &str, schedule: &str) -> (u16, Value) {
    call(
        router,
        "CreateMaintenanceWindow",
        &json!({
            "Name": name,
            "Schedule": schedule,
            "Duration": 3,
            "Cutoff": 1,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await
}

/// The windows a describe narrowed to `name` reports.
async fn described(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let (status, response) = call(
        router,
        "DescribeMaintenanceWindows",
        &json!({ "Filters": [{ "Key": "Name", "Values": [name] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the describe of {name:?} must be served, got {status} {response}"
    );
    response["WindowIdentities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: a `cron(...)` body the parser will not read is refused with the position the parse
/// failed at and the rule it broke, and no window is created.
#[tokio::test]
async fn a_malformed_cron_names_the_position_and_the_rule() {
    let router = router();

    // (case, the schedule sent, the position reported, the detail reported)
    let rows: [(&str, &str, i64, &str); 8] = [
        (
            "no specific value where the parser reads an hour",
            "cron(0 25 ? * MON *)",
            1,
            NO_SPECIFIC_VALUE,
        ),
        (
            "no specific value where it reads a minute",
            "cron(? 8 1 * MON *)",
            1,
            NO_SPECIFIC_VALUE,
        ),
        (
            "a minute past the values a minute admits",
            "cron(99 8 1 * MON *)",
            -1,
            "Minute and Second values must be between 0 and 59.",
        ),
        (
            "an hour past the values an hour admits",
            "cron(0 0 99 * MON *)",
            -1,
            "Hour values must be between 0 and 23.",
        ),
        (
            "a day of the month past the values it admits",
            "cron(0 0 0 99 MON *)",
            -1,
            "Day of month values must be between 1 and 31.",
        ),
        (
            "characters the grammar does not read",
            "cron(zzz 8 1 * MON *)",
            0,
            // The parser reads the expression in upper case — a named month or weekday is one
            // of a fixed set of names, matched whatever case it was written in — so the token
            // is quoted as the parser saw it.
            "Illegal characters for this position: 'ZZZ'",
        ),
        (
            "a body of five fields",
            "cron(0 8 1 * ?)",
            9,
            UNEXPECTED_END,
        ),
        (
            "a body of one field",
            "cron(0)",
            1,
            UNEXPECTED_END,
        ),
    ];

    for (index, (case, schedule, position, detail)) in rows.into_iter().enumerate() {
        let name = format!("cron-refusal-{index}");
        let expected = invalid_cron_message(position, detail);
        let (status, response) = create(&router, &name, schedule).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {schedule:?} is no schedule, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the validation refusal, got {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // A refused create writes nothing: no window of that name exists.
        let held = described(&router, &name).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused create must leave no window, got {held:?}"
        );
    }
}

/// The same rule holds for an UPDATE of a window's schedule, and the window keeps the
/// schedule it already held.
#[tokio::test]
async fn an_update_to_a_malformed_cron_is_refused_the_same_way() {
    let router = router();
    let name = "cron-refusal-update";
    let kept = "cron(0 16 ? * TUE *)";

    let (status, created) = create(&router, name, kept).await;
    assert_eq!(
        status, 200,
        "the window must be created first, got {status} {created}"
    );
    let window_id = created["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a WindowId, got {created}"))
        .to_string();

    // (case, the schedule the update sends, the position, the detail)
    let rows: [(&str, &str, i64, &str); 3] = [
        (
            "no specific value where the parser reads an hour",
            "cron(0 25 ? * MON *)",
            1,
            NO_SPECIFIC_VALUE,
        ),
        (
            "a month past the values a month admits",
            "cron(0 0 0 1 99 *)",
            -1,
            "Month values must be between 1 and 12.",
        ),
        (
            "a body of five fields",
            "cron(0 8 1 * ?)",
            9,
            UNEXPECTED_END,
        ),
    ];

    for (case, schedule, position, detail) in rows {
        let expected = invalid_cron_message(position, detail);
        let (status, response) = call(
            &router,
            "UpdateMaintenanceWindow",
            &json!({ "WindowId": window_id, "Schedule": schedule }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the validation refusal, got {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    let (status, read) = call(
        &router,
        "GetMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must still be readable, got {status} {read}"
    );
    assert_eq!(
        read["Schedule"].as_str(),
        Some(kept),
        "the refused updates must have left the stored schedule as it was, got {read}"
    );
}

/// A documented expression the parser DOES read is still served: the refusal above is about
/// the bodies that fail, not about the form. The rows span the documented cron forms,
/// including this service's own dialect wildcards.
#[tokio::test]
async fn an_expression_the_parser_reads_is_still_served() {
    let router = router();

    // (case, schedule)
    let rows: [(&str, &str); 8] = [
        ("a cron on one weekday", "cron(0 16 ? * TUE *)"),
        ("a cron over a range of weekdays", "cron(0 18 ? * MON-FRI *)"),
        ("a cron over a step of minutes", "cron(0/15 * * * ? *)"),
        ("a cron on a day of the month", "cron(30 7 15 * ? *)"),
        ("a cron on the last day of the month", "cron(0 8 L * ? *)"),
        ("a cron on the nearest weekday", "cron(0 8 15W * ? *)"),
        ("a cron on the third Tuesday", "cron(30 23 ? * TUE#3 *)"),
        ("a cron naming a month", "cron(0 2 1 JAN ? *)"),
    ];

    for (index, (case, schedule)) in rows.into_iter().enumerate() {
        let name = format!("cron-served-{index}");
        let (status, response) = create(&router, &name, schedule).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {schedule:?} is a schedule this service reads, got {status} \
             {response}"
        );
        let held = described(&router, &name).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: the window must be the one the create minted, got {held:?}"
        );
        assert_eq!(
            held[0]["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the window the create named, got {held:?}"
        );
    }
}
