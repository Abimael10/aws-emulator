// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a code security scan configuration reports about the
//! schedule its periodic scans run on, and for its tag map.
//!
//! The rules under test:
//!
//! * A periodic scan runs on a schedule, so a configuration whose periodic settings name
//!   a frequency reports the schedule expression those scans run on: the expression the
//!   request named, or — when the request named none — the one the service chose for that
//!   frequency, the same for every configuration of that frequency and stable across
//!   reads. A frequency that names no schedule reports no expression, and settings that
//!   name no periodic scanning at all report nothing.
//! * A resource's tag map is part of the resource, so a read reports it as a map — empty
//!   for a configuration carrying no tags — rather than leaving the member out.
//!
//! Every request goes through the real `MultiServiceRouter` — routing, restJson1
//! deserialization, modelled-constraint validation, the handler, and restJson1
//! serialization — so what is asserted here is the wire behaviour a client sees.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::filters_and_enablement::{post_in, router};

/// The modelled length of a `FrequencyExpression`.
/// Source: `smithy_query(query="shape", shape="FrequencyExpression")` — `@length 1..256`.
const EXPRESSION_LENGTH: (usize, usize) = (1, 256);

/// Create a configuration with `configuration` as its scan settings, and `tags` when the
/// case asks for a `tags` member at all. Answers the minted ARN.
async fn create(
    router: &MultiServiceRouter,
    case: &str,
    configuration: &Value,
    tags: Option<&Value>,
) -> String {
    let mut body = json!({
        "name": "periodic-schedule",
        "level": "ACCOUNT",
        "configuration": configuration.clone(),
    });
    if let Some(tags) = tags {
        body["tags"] = tags.clone();
    }
    let (status, error_type, created) = post_in(
        router,
        "us-east-1",
        "/codesecurity/scan-configuration/create",
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCodeSecurityScanConfiguration: expected 200, got {status} / \
         {created} ({error_type:?})"
    );
    created["scanConfigurationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no scanConfigurationArn in {created}"))
        .to_string()
}

/// Read a configuration back; answers the 200 body.
async fn get(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, error_type, read) = post_in(
        router,
        "us-east-1",
        "/codesecurity/scan-configuration/get",
        &json!({ "scanConfigurationArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCodeSecurityScanConfiguration: expected 200, got {status} / {read} \
         ({error_type:?})"
    );
    read
}

/// The six fields of an AWS cron expression `cron(<minutes> <hours> <day-of-month> <month>
/// <day-of-week> <year>)`, or `None` for a string that is not one.
fn cron_fields(expression: &str) -> Option<Vec<String>> {
    let inner = expression
        .strip_prefix("cron(")
        .and_then(|rest| rest.strip_suffix(')'))?;
    let fields: Vec<String> = inner.split_whitespace().map(str::to_string).collect();
    (fields.len() == 6).then_some(fields)
}

/// Assert `expression` is a schedule a scan of `frequency` can run on: a well-formed AWS
/// cron expression inside the modelled `FrequencyExpression` length whose day fields name
/// the day the frequency repeats on — the day of the week for a weekly scan and the day of
/// the month for a monthly one — with the other day field left as the `?` AWS cron
/// requires, and a fixed time of day rather than a repeat inside the day.
///
/// Source: `smithy_query(query="shape", shape="PeriodicScanConfiguration")` — a provided
/// `frequencyExpression` must match the `frequency`, and the service chooses the day when
/// none is provided.
pub(crate) fn assert_schedule_matches(case: &str, frequency: &str, expression: &str) {
    assert!(
        expression.len() >= EXPRESSION_LENGTH.0 && expression.len() <= EXPRESSION_LENGTH.1,
        "case {case:?}: expected a FrequencyExpression of {EXPRESSION_LENGTH:?} characters, got \
         {expression:?}"
    );
    let fields = cron_fields(expression).unwrap_or_else(|| {
        panic!("case {case:?}: expected a six-field cron expression, got {expression:?}")
    });
    let fixed = |field: &str| field.chars().all(|char| char.is_ascii_digit());
    assert!(
        fixed(&fields[0]) && fixed(&fields[1]),
        "case {case:?}: expected a fixed time of day, got {expression:?}"
    );
    let (day_of_month, day_of_week) = (fields[2].as_str(), fields[4].as_str());
    match frequency {
        "WEEKLY" => {
            assert!(
                day_of_month == "?" && fixed(day_of_week),
                "case {case:?}: a weekly schedule names the day of the week and leaves the day \
                 of the month as '?', got {expression:?}"
            );
        }
        "MONTHLY" => {
            assert!(
                day_of_week == "?" && fixed(day_of_month),
                "case {case:?}: a monthly schedule names the day of the month and leaves the day \
                 of the week as '?', got {expression:?}"
            );
        }
        other => panic!("case {case:?}: no schedule is expected for frequency {other:?}"),
    }
}

/// The periodic settings of a read configuration, or `Value::Null` when the member is
/// absent.
fn periodic(read: &Value) -> Value {
    read["configuration"]
        .get("periodicScanConfiguration")
        .cloned()
        .unwrap_or(Value::Null)
}

/// RULE: a configuration whose periodic settings name a frequency without a schedule
/// expression reports the expression the service chose for that frequency — present, of
/// the modelled shape, matching the frequency and the same on every read; a frequency that
/// names no schedule reports no expression at all.
///
/// Rows span every `PeriodicScanFrequency` token and the periodic settings left out
/// entirely.
/// Source: `smithy_query(query="shape", shape="PeriodicScanFrequency")` — `WEEKLY`,
/// `MONTHLY`, `NEVER`.
#[tokio::test]
async fn a_frequency_without_an_expression_reports_the_schedule_the_service_chose() {
    // (case, the periodic settings as sent, the frequency a schedule is owed for)
    let rows: [(&str, Value, Option<&str>); 4] = [
        ("weekly", json!({ "frequency": "WEEKLY" }), Some("WEEKLY")),
        ("monthly", json!({ "frequency": "MONTHLY" }), Some("MONTHLY")),
        // `NEVER` turns periodic scanning off: there is no schedule to express.
        ("never", json!({ "frequency": "NEVER" }), None),
        // No periodic settings at all: the member stays absent, schedule and frequency
        // both.
        ("absent", Value::Null, None),
    ];

    for (case, settings, owed) in rows {
        let router = router();
        let mut configuration = json!({ "ruleSetCategories": ["SAST"] });
        if !settings.is_null() {
            configuration["periodicScanConfiguration"] = settings.clone();
        }
        let arn = create(&router, case, &configuration, None).await;
        let read = get(&router, case, &arn).await;

        match owed {
            None => {
                let reported = periodic(&read);
                assert_eq!(
                    reported, settings,
                    "case {case:?}: expected the periodic settings as sent, got {reported}"
                );
                assert_eq!(
                    reported.get("frequencyExpression"),
                    None,
                    "case {case:?}: expected no frequencyExpression, got {reported}"
                );
            }
            Some(frequency) => {
                let reported = periodic(&read);
                assert_eq!(
                    reported["frequency"],
                    json!(frequency),
                    "case {case:?}: expected the frequency as sent, got {reported}"
                );
                let expression = reported["frequencyExpression"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a chosen frequencyExpression, got {reported}")
                });
                assert_schedule_matches(case, frequency, expression);

                // A read reports the stored schedule, so a second read of the same
                // configuration reports it again; a second configuration of the same
                // frequency gets a schedule of its own, which satisfies the same rule.
                let again = get(&router, case, &arn).await;
                assert_eq!(
                    periodic(&again),
                    reported,
                    "case {case:?}: a repeated read must report the same schedule, got {again}"
                );
                let other_arn = create(&router, case, &configuration, None).await;
                let other = get(&router, case, &other_arn).await;
                let other_expression = periodic(&other)["frequencyExpression"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a chosen frequencyExpression, got {other}")
                    });
                assert_schedule_matches(case, frequency, &other_expression);
            }
        }
    }
}

/// RULE: a schedule expression the request named is the configuration's own — the service
/// chooses one only when the request named none, so a caller's expression is reported back
/// unchanged.
///
/// Rows differ along the expression's form and length, including the modelled
/// `FrequencyExpression` bounds (1 and 256 characters) and expressions this emulator would
/// never choose itself.
#[tokio::test]
async fn an_expression_the_request_named_is_reported_back_unchanged() {
    let rows: [(&str, &str, String); 5] = [
        (
            "weekly, a day the service would not choose",
            "WEEKLY",
            "cron(30 7 ? * 5 *)".to_string(),
        ),
        (
            "monthly, the last day of the month",
            "MONTHLY",
            "cron(0 23 L * ? *)".to_string(),
        ),
        (
            "never, with an expression of its own",
            "NEVER",
            "cron(0 0 1 1 ? 2030)".to_string(),
        ),
        // The shortest expression the member's modelled length admits.
        ("the shortest expression", "WEEKLY", "x".to_string()),
        // The longest expression the member's modelled length admits (256 characters).
        (
            "the longest expression",
            "MONTHLY",
            "y".repeat(EXPRESSION_LENGTH.1),
        ),
    ];

    for (case, frequency, expression) in rows {
        let router = router();
        let configuration = json!({
            "ruleSetCategories": ["IAC"],
            "periodicScanConfiguration": {
                "frequency": frequency,
                "frequencyExpression": expression,
            },
        });
        let arn = create(&router, case, &configuration, None).await;
        let read = get(&router, case, &arn).await;
        assert_eq!(
            read["configuration"], configuration,
            "case {case:?}: expected the configuration as sent, got {read}"
        );
        assert_eq!(
            periodic(&read)["frequencyExpression"],
            json!(expression),
            "case {case:?}: expected the expression the request named, got {read}"
        );
    }
}

/// RULE: a code security scan configuration reports its tag map on every read — the tags
/// it carries, and an empty map when it carries none — so the shape of the answer never
/// depends on whether tags were set.
///
/// Rows differ along the `tags` member being left out, sent empty and sent with entries,
/// including the modelled `MapKey`/`MapValue` length bounds.
/// Source: `smithy_query(query="shape", shape="TagMap")` — keys of 1..128 characters,
/// values of up to 256.
#[tokio::test]
async fn the_tag_map_is_always_reported() {
    let rows: [(&str, Option<Value>, Value); 5] = [
        ("no tags member at all", None, json!({})),
        ("an empty tags map", Some(json!({})), json!({})),
        (
            "one tag",
            Some(json!({ "owner": "security-team" })),
            json!({ "owner": "security-team" }),
        ),
        (
            "several tags",
            Some(json!({ "owner": "security-team", "costCenter": "4711" })),
            json!({ "owner": "security-team", "costCenter": "4711" }),
        ),
        (
            "the shortest key and the longest value",
            Some(json!({ "k": "v".repeat(256) })),
            json!({ "k": "v".repeat(256) }),
        ),
    ];

    for (case, tags, expected) in rows {
        let router = router();
        let configuration = json!({ "ruleSetCategories": ["SCA"] });
        let arn = create(&router, case, &configuration, tags.as_ref()).await;
        let read = get(&router, case, &arn).await;
        let reported = read.get("tags").cloned().unwrap_or(Value::Null);
        assert!(
            reported.is_object(),
            "case {case:?}: tags must be a present object, got {reported}"
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected tags {expected}, got {reported}"
        );
    }
}
