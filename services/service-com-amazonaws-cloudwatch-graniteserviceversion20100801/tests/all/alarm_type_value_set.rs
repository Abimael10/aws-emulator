// SPDX-License-Identifier: CC0-1.0

//! How a refusal of a value outside the enumerated type a LIST's elements are drawn from is
//! stated, over the alarm reads that take one.
//!
//! The rule under test: **a constraint broken by the ELEMENTS of a list is stated at the LIST
//! MEMBER — quoting the list that member held and nesting the element shape's whole value set
//! inside `Member must satisfy constraint: [ … ]` — in the request-validation class, on every
//! operation whose model declares none of this service's parameter faults; and the value set a
//! clause enumerates is the one the service PUBLISHES for that type.** The framework checks an
//! element's own shape per element and reports it at that element with the one constraint that
//! failed, where AWS reports one validation error about one member: two offending elements of
//! one list are one clause, and the set a caller has to choose from is what makes the refusal
//! actionable.
//!
//! What the published set does NOT decide is which values a member ADMITS: that is the enum the
//! member targets, so a filter naming a kind of alarm the set does not enumerate is still
//! served — the last suite below is that half of the rule.
//!
//! The rows differ along what the rule generalises over: which OPERATION carried the list (two
//! reads whose model declares only a token fault), how the value misses the type (a word that
//! names no kind of alarm, a declared value mis-cased, a declared value with a suffix), WHERE
//! in the list it sat, how many elements the list held and how many of them were outside the
//! type.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, ok};

/// The error SHAPE the body of a request-validation refusal names.
const VALIDATION_SHAPE: &str = "ValidationException";

/// The member both reads take the kinds of alarm to report under, as the validating walk
/// spells a member path.
const ALARM_TYPES_MEMBER: &str = "alarmTypes";

/// The kinds of alarm a refusal of this service ENUMERATES, in the order it lists them: the
/// value set the service publishes for the alarm type. The on-disk model declares a third kind
/// beside these two (`smithy_query(query="shape",shape="AlarmType")`), which a refusal does not
/// list and a request may still name.
const PUBLISHED_ALARM_TYPES: &str = "MetricAlarm, CompositeAlarm";

/// The kind of alarm the model declares that the published set above does not list.
const UNPUBLISHED_ALARM_TYPE: &str = "LogAlarm";

/// The whole refusal a list of alarm types holding a value outside the type earns: the list as
/// the request carried it, the member it arrived on, and the element shape's published set.
fn refusal(submitted: &[&str]) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at '{ALARM_TYPES_MEMBER}' failed to satisfy \
         constraint: Member must satisfy constraint: [Member must satisfy enum value set: \
         [{PUBLISHED_ALARM_TYPES}]]",
        submitted.join(", ")
    )
}

/// The request `operation` takes for the alarm types `types`.
fn asking_for(types: &[&str]) -> Value {
    json!({"AlarmTypes": types})
}

#[tokio::test]
async fn an_alarm_type_outside_the_published_set_is_refused_at_the_list_member() {
    // (case, operation, the alarm types the request carries)
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "a word that names no kind of alarm",
            "DescribeAlarms",
            vec!["NotARealType"],
        ),
        (
            "a declared value mis-cased",
            "DescribeAlarms",
            vec!["metricalarm"],
        ),
        (
            "a declared value with a suffix",
            "DescribeAlarms",
            vec!["MetricAlarms"],
        ),
        (
            "an unknown kind in the SECOND position, after a declared one",
            "DescribeAlarms",
            vec!["MetricAlarm", "NotARealType"],
        ),
        (
            "two unknown kinds in one list, which is ONE validation error",
            "DescribeAlarms",
            vec!["NotARealType", "NeitherIsThis"],
        ),
        (
            "the same list on the alarm HISTORY, whose model declares no parameter fault either",
            "DescribeAlarmHistory",
            vec!["NotARealType"],
        ),
        (
            "an unknown kind in the third position of the history's list",
            "DescribeAlarmHistory",
            vec!["MetricAlarm", "CompositeAlarm", "NotARealType"],
        ),
    ];

    let router = build_router();
    for (case, operation, types) in rows {
        let (status, body) = call(&router, operation, &asking_for(&types)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, VALIDATION_SHAPE),
            "case {case:?} ({operation}): expected HTTP 400 {VALIDATION_SHAPE}, got {status} \
             with {body}"
        );
        let expected = refusal(&types);
        let stated = error_message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?} ({operation}): expected {expected:?}, got {stated:?}"
        );
        // Stated separately, because the SET is what a caller repairs the request from: the
        // clause enumerates what the service publishes and nothing else.
        assert!(
            !stated.contains(UNPUBLISHED_ALARM_TYPE),
            "case {case:?} ({operation}): the enumerated set must not name \
             {UNPUBLISHED_ALARM_TYPE}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn every_alarm_type_the_model_declares_is_still_served() {
    // The other half of the rule: the published set decides what a REFUSAL enumerates, never
    // what a member admits. Each row is a list of kinds the model declares, including the one
    // the published set does not list, over both reads.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("the kind the published set leaves out", vec!["LogAlarm"]),
        ("a metric alarm", vec!["MetricAlarm"]),
        ("a composite alarm", vec!["CompositeAlarm"]),
        (
            "every kind the model declares",
            vec!["MetricAlarm", "CompositeAlarm", "LogAlarm"],
        ),
    ];

    let router = build_router();
    for (case, types) in rows {
        for operation in ["DescribeAlarms", "DescribeAlarmHistory"] {
            let served = ok(&router, operation, &asking_for(&types)).await;
            assert!(
                served.is_object(),
                "case {case:?} ({operation}): expected the read to be served, got {served}"
            );
        }
    }
}
