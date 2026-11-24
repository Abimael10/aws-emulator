// SPDX-License-Identifier: CC0-1.0

//! Two refusals of an experiment template, end to end over HTTP: the source a stop
//! condition may name, and the windows an experiment report configuration may carry.
//!
//! The rules under test, each stated about a TEMPLATE and so judged alike by
//! `CreateExperimentTemplate` and `UpdateExperimentTemplate`:
//!
//! * a stop condition names one of the two sources FIS defines, and any other value is a
//!   400 `ValidationException` whose message is the fixed sentence FIS words for it with
//!   the offending source bracketed in it;
//! * `preExperimentDuration` and `postExperimentDuration` each name a span of time, so a
//!   value that is no duration is a 400 `ValidationException` naming the member that
//!   carries it, and the template is left exactly as it was.
//!
//! The rows differ along what each rule generalises over: for the source, which unknown
//! value is sent (a neighbouring FIS-style source, a differently cased form of an admitted
//! one, a word, a value carrying punctuation and brackets of its own) beside the two
//! sources FIS does define; for the windows, WHICH member carries the defect, and which
//! kind of non-duration it is (prose, a bare designator, a date, a calendar-dependent
//! duration whose span is not fixed) beside the durations the rule admits.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{
    create_template, emulator, send, token, Answer, FALLBACK_REGION,
};

/// A well-formed IAM role ARN: every row here is about something other than the role.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-report-window-suite";

/// A CloudWatch alarm ARN, for the row whose stop condition names the alarm source.
const ALARM_ARN: &str = "arn:aws:cloudwatch:us-east-1:000000000000:alarm:fis-report-window";

/// The two sources FIS defines for a stop condition: "Specify `aws:cloudwatch:alarm` if the
/// stop condition is defined by a CloudWatch alarm. Specify `none` if there is no stop
/// condition." (`StopCondition$source`, read with
/// `smithy_query(query="doc",shape="StopCondition",member="source")`). Spelled out here
/// rather than imported so a changed set in `src/state.rs` fails this suite.
const ADMITTED_SOURCES: [&str; 2] = ["none", "aws:cloudwatch:alarm"];

/// `CreateExperimentTemplate` with the members every create requires, plus `members`.
async fn create_with(router: &MultiServiceRouter, label: &str, members: &Value) -> Answer {
    let mut body = json!({
        "clientToken": token(label),
        "description": format!("report window suite: {label}"),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": {},
        "actions": { "wait": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT5M" } } }
    });
    merge(&mut body, members);
    create_template(router, body).await
}

/// `UpdateExperimentTemplate`: PATCH the template `id` names with `members`.
async fn update_with(router: &MultiServiceRouter, id: &str, members: Value) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/experimentTemplates/{id}"),
        Some(members),
        FALLBACK_REGION,
    )
    .await
}

/// The template `id` names, as `GetExperimentTemplate` reports it.
async fn read_template(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let answer = send(
        router,
        "GET",
        &format!("/experimentTemplates/{id}"),
        None,
        FALLBACK_REGION,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from GetExperimentTemplate, got {} with {:?}",
        answer.status, answer.body
    );
    answer.member("experimentTemplate").clone()
}

/// Copy every member of `members` into `body`.
fn merge(body: &mut Value, members: &Value) {
    let (Some(body), Some(members)) = (body.as_object_mut(), members.as_object()) else {
        panic!("both the body and the row's members are JSON objects");
    };
    for (name, value) in members {
        body.insert(name.clone(), value.clone());
    }
}

/// The `message` member of an answer, as a refused call carries it.
fn message(answer: &Answer) -> String {
    answer.body["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A baseline template this suite's update rows start from: admissible, and carrying a
/// report configuration with an output, a data source and both windows set, so a row that
/// replaces the configuration has a stored one to leave untouched.
fn baseline(label: &str) -> Value {
    json!({
        "clientToken": token(&format!("baseline-{label}")),
        "description": format!("report window baseline: {label}"),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": {},
        "actions": { "wait": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT5M" } } },
        "experimentReportConfiguration": {
            "outputs": { "s3Configuration": { "bucketName": "fis-reports", "prefix": "windows" } },
            "dataSources": { "cloudWatchDashboards": [{ "dashboardIdentifier": "fis-dash" }] },
            "preExperimentDuration": "PT1M",
            "postExperimentDuration": "PT1M"
        }
    })
}

/// Create the baseline template and return its id.
async fn baseline_template(router: &MultiServiceRouter, case: &str, label: &str) -> String {
    let answer = create_template(router, baseline(label)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 for the baseline template, got {} with {:?}",
        answer.status, answer.body
    );
    answer.member("experimentTemplate")["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no experimentTemplate.id in {:?}", answer.body))
        .to_string()
}

/// Assert `answer` is the 400 `ValidationException` whose message is exactly `expected`.
fn assert_refused(case: &str, call: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 from {call}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ValidationException",
        "case {case:?}: expected error code \"ValidationException\" from {call}, got {:?}",
        answer.body
    );
    let actual = message(answer);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the {call} message to be {expected:?}, got {actual:?}"
    );
}

/// RULE: a stop condition's `source` is one of the two sources FIS defines, and any other
/// value earns the sentence FIS words for it — the fixed text with the offending source
/// bracketed in it — from the create and from the update alike.
#[tokio::test]
async fn an_undefined_stop_condition_source_is_refused_as_fis_words_it() {
    // The rows differ along the SHAPE of the unknown value, not along the resource it
    // names: a neighbouring FIS-style source, a differently cased admitted one, a bare
    // word, and a value carrying punctuation and brackets of its own.
    let unknown_sources = [
        "aws:cloudwatch:metric",
        "aws:fis:alarm",
        "AWS:CloudWatch:Alarm",
        "None",
        "alarm",
        "[none]",
    ];

    for (index, source) in unknown_sources.iter().enumerate() {
        let case = format!("source {source:?}");
        let router = emulator();
        let expected = format!("Invalid source for Stop Condition: [{source}].");
        let members = json!({ "stopConditions": [{ "source": source }] });

        let created = create_with(&router, &format!("source-{index}"), &members).await;
        assert_refused(&case, "CreateExperimentTemplate", &created, &expected);

        let id = baseline_template(&router, &case, &format!("source-{index}")).await;
        let before = read_template(&router, &case, &id).await;
        let updated = update_with(&router, &id, members).await;
        assert_refused(&case, "UpdateExperimentTemplate", &updated, &expected);
        let after = read_template(&router, &case, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must change nothing, got {after:?}"
        );
    }

    // The two sources the rule admits: neither is refused, and the alarm source carries
    // the alarm's ARN as FIS requires of it.
    for (index, source) in ADMITTED_SOURCES.iter().enumerate() {
        let case = format!("admitted source {source:?}");
        let router = emulator();
        let condition = if *source == "aws:cloudwatch:alarm" {
            json!({ "source": source, "value": ALARM_ARN })
        } else {
            json!({ "source": source })
        };
        let members = json!({ "stopConditions": [condition] });
        let created = create_with(&router, &format!("admitted-{index}"), &members).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateExperimentTemplate, got {} with {:?}",
            created.status, created.body
        );
    }
}

/// Values that are no ISO 8601 duration naming a fixed span, each inside
/// `ReportConfigurationDuration`'s modelled bounds (`@length(0..32)`, `pattern ^[\S]+$`,
/// read with `smithy_query(query="shape",shape="ReportConfigurationDuration")`) so only the
/// server-side rule can refuse them. `P1Y` and `P1M` ARE durations, but their length depends
/// on the calendar, so they name no fixed window either.
const NOT_DURATIONS: [&str; 8] = [
    "not-a-duration",
    "5minutes",
    "PT",
    "P",
    "2025-07-01",
    "pt1m",
    "P1Y",
    "P1M",
];

/// Durations the rule admits, spanning the grammar's fields: minutes, hours, a sub-second
/// fraction, a zero-length span and a combined form.
const DURATIONS: [&str; 5] = ["PT1M", "PT3H", "PT0.5S", "PT0S", "P1DT2H3M4S"];

/// The two members of an experiment report configuration that name a window, with the
/// sentence each one's refusal carries.
const WINDOW_MEMBERS: [&str; 2] = ["preExperimentDuration", "postExperimentDuration"];

/// The refusal FIS words for a report window that is no duration.
fn window_refusal(member: &str) -> String {
    format!(
        "The {member} parameter value for the experimentReportConfiguration is invalid. \
         Specify a valid duration value and try again."
    )
}

/// The report configuration carrying `value` under `member`, with the output and data
/// source a usable configuration names beside it.
fn report_configuration(member: &str, value: &str) -> Value {
    json!({
        "experimentReportConfiguration": {
            "outputs": { "s3Configuration": { "bucketName": "fis-reports", "prefix": "windows" } },
            "dataSources": { "cloudWatchDashboards": [{ "dashboardIdentifier": "fis-dash" }] },
            member: value
        }
    })
}

/// RULE: each window of an experiment report configuration names a span of time, so a value
/// that is no duration is refused with a 400 `ValidationException` naming that member —
/// by the create and by the update alike — and nothing is stored.
#[tokio::test]
async fn a_report_window_that_is_no_duration_is_refused_and_stores_nothing() {
    for member in WINDOW_MEMBERS {
        let expected = window_refusal(member);
        for (index, value) in NOT_DURATIONS.iter().enumerate() {
            let case = format!("{member} = {value:?}");
            let label = format!("{member}-{index}");
            let router = emulator();
            let members = report_configuration(member, value);

            let created = create_with(&router, &label, &members).await;
            assert_refused(&case, "CreateExperimentTemplate", &created, &expected);

            let id = baseline_template(&router, &case, &label).await;
            let before = read_template(&router, &case, &id).await;
            let updated = update_with(&router, &id, members).await;
            assert_refused(&case, "UpdateExperimentTemplate", &updated, &expected);
            let after = read_template(&router, &case, &id).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused update must change nothing, got {after:?}"
            );
        }
    }
}

/// RULE: the same rule ADMITS a window that is a duration, on either member, and the
/// template stores it — so a later read reports the window the request named.
#[tokio::test]
async fn a_report_window_that_is_a_duration_is_stored_and_read_back() {
    for member in WINDOW_MEMBERS {
        for (index, value) in DURATIONS.iter().enumerate() {
            let case = format!("{member} = {value:?}");
            let label = format!("ok-{member}-{index}");
            let router = emulator();
            let members = report_configuration(member, value);

            let created = create_with(&router, &label, &members).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected HTTP 200 from CreateExperimentTemplate, got {} with {:?}",
                created.status, created.body
            );
            let created_id = created.member("experimentTemplate")["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no id in {:?}", created.body))
                .to_string();
            let stored = read_template(&router, &case, &created_id).await;
            assert_eq!(
                stored["experimentReportConfiguration"][member]
                    .as_str()
                    .unwrap_or_default(),
                *value,
                "case {case:?}: expected the created template to carry the window, got {:?}",
                stored["experimentReportConfiguration"]
            );

            let id = baseline_template(&router, &case, &label).await;
            let updated = update_with(&router, &id, members).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected HTTP 200 from UpdateExperimentTemplate, got {} with {:?}",
                updated.status, updated.body
            );
            let after = read_template(&router, &case, &id).await;
            assert_eq!(
                after["experimentReportConfiguration"][member]
                    .as_str()
                    .unwrap_or_default(),
                *value,
                "case {case:?}: expected the updated template to carry the window, got {:?}",
                after["experimentReportConfiguration"]
            );
        }
    }
}

/// RULE: the windows are judged in the order the model declares them, so a configuration
/// whose two windows are both unreadable earns the `preExperimentDuration` refusal — the
/// same one from both calls.
#[tokio::test]
async fn both_windows_unreadable_reports_the_first_member_the_model_declares() {
    let case = "both windows unreadable";
    let router = emulator();
    let members = json!({
        "experimentReportConfiguration": {
            "preExperimentDuration": "not-a-duration",
            "postExperimentDuration": "also-not-a-duration"
        }
    });
    let expected = window_refusal("preExperimentDuration");

    let created = create_with(&router, "both-windows", &members).await;
    assert_refused(case, "CreateExperimentTemplate", &created, &expected);

    let id = baseline_template(&router, case, "both-windows").await;
    let updated = update_with(&router, &id, members).await;
    assert_refused(case, "UpdateExperimentTemplate", &updated, &expected);
}
