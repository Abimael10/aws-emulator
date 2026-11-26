// SPDX-License-Identifier: CC0-1.0

//! A composite alarm driven the way a CLIENT drives one: each child moved by its own
//! `SetAlarmState` call, with the client's own reads in between.
//!
//! Two rules, each asserted over a table.
//!
//! 1. **A composite alarm's `StateValue` is its whole `AlarmRule` applied to the CURRENT states
//!    of the alarms the rule references, re-derived whenever one of those states changes** — "An
//!    expression that specifies which other alarms are to be evaluated to determine this
//!    composite alarm's state… You can use operators (AND, OR and NOT) to combine multiple
//!    functions in a single expression", and `ALARM(a) AND ALARM(b)` "specifies that the
//!    composite alarm goes into ALARM state only if both […] alarms are in ALARM state"
//!    (`smithy_query(query="doc",shape="PutCompositeAlarmInput",member="AlarmRule")`). So a
//!    combined rule reaches `ALARM` once — and only once — every function in it holds, however
//!    many separate calls the client took to get the children there, and it leaves `ALARM` again
//!    as soon as one of them stops holding. The rows differ along what the rule generalises
//!    over: the combinator (`AND`, `OR`, `NOT`, parentheses), the state function each term
//!    tests, how many children the rule reads, which child completes it, and the order the
//!    client drives them in.
//! 2. **A composite alarm that names an `ActionsSuppressor` reports which of the modelled
//!    reasons is withholding its actions, and the one that follows the suppressor LEAVING
//!    `ALARM` is `ExtensionPeriod`** — "When the value is `ExtensionPeriod`, it means that the
//!    actions are suppressed because the composite alarm is waiting after the suppressor alarm
//!    went out of the `ALARM` state. The maximum waiting time is as specified in
//!    `ActionsSuppressorExtensionPeriod`. After this time, the composite alarm performs its
//!    actions."
//!    (`smithy_query(query="doc",shape="CompositeAlarm",member="ActionsSuppressedBy")`) — so the
//!    suppression is a timed machine rather than a boolean read off the suppressor's current
//!    state: the field is reported for the configured period while the composite alarm's own
//!    state stays where its rule put it, and only then stops being reported. The rows differ
//!    along the configured extension period (including the one that runs out and the one that is
//!    zero) and along whether the suppressor was already in `ALARM` when the rule completed.
//!
//! Every child here carries the DEFAULT missing-data handling (the configuration a client writes
//! when it says nothing about it) over a series nothing publishes into, so what the rules are
//! asserted over is the state the client forced and nothing else. Every expectation is awaited
//! on the CONDITION — a state that must hold is watched over a span of the service's own clock
//! ticks, a state that must be reached is polled for until a generous deadline — so no row
//! depends on how fast the host runs.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, ok, LOOK_INTERVAL, QUIET_WATCH, WORK_WAIT};

/// The modelled `StateValue` values these rows name.
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The children every rule below reads, by name.
const KID_A: &str = "kid-a";
const KID_B: &str = "kid-b";
const KID_C: &str = "kid-c";

/// The composite alarm every row creates.
const COMPOSITE: &str = "composite-under-test";

/// The alarm a suppressed row names as its actions suppressor.
const SUPPRESSOR: &str = "maintenance-window";

/// A child metric alarm of `name`, over a series of its own that nothing publishes into.
///
/// It names no `TreatMissingData`, which is the configuration of a client that says nothing
/// about it ("If `TreatMissingData` is not specified, `missing` is used",
/// `smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`) — so these
/// rows assert the rules over the same child configuration a client writes by default.
async fn given_child(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "MyApp/CompositeDrive",
            "MetricName": "Load",
            "Dimensions": [{"Name": "Child", "Value": name}],
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
        }),
    )
    .await;
}

/// Force the alarm `name` into `state`, the way a client drives a scenario.
async fn force(router: &MultiServiceRouter, name: &str, state: &str) {
    ok(
        router,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "driven by the test",
        }),
    )
    .await;
}

/// The composite alarm `name` as `DescribeAlarms` reports it: "If you specify CompositeAlarms,
/// the operation returns only a list of composite alarms"
/// (`smithy_query(query="doc",shape="DescribeAlarmsInput",member="AlarmTypes")`).
async fn composite(router: &MultiServiceRouter, name: &str) -> Value {
    ok(
        router,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["CompositeAlarm"]}),
    )
    .await["CompositeAlarms"][0]
        .clone()
}

/// What the string member `member` of the composite alarm `name` reports, or `None` where the
/// alarm does not report that member at all.
async fn reported(router: &MultiServiceRouter, name: &str, member: &str) -> Option<String> {
    composite(router, name).await[member]
        .as_str()
        .map(ToString::to_string)
}

/// Look at `name` until its `member` reports `wanted`, and answer the alarm as it then stood — a
/// wait for the CONDITION, with a deadline only so a failure is a failure rather than a hang.
async fn until_reports(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    member: &str,
    wanted: Option<&str>,
) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let alarm = composite(router, name).await;
        if alarm[member].as_str() == wanted {
            return alarm;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must report {member} {wanted:?}; it last reported {alarm}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every distinct value the member `member` of `name` took over [`QUIET_WATCH`], in the order it
/// took them: how a row shows a value it expects the alarm to HOLD, across several of the
/// service's own clock ticks.
async fn over_the_watch(
    router: &MultiServiceRouter,
    name: &str,
    member: &str,
) -> Vec<Option<String>> {
    let deadline = Instant::now() + QUIET_WATCH;
    let mut seen: Vec<Option<String>> = Vec::new();
    while Instant::now() < deadline {
        let held = reported(router, name, member).await;
        if seen.last() != Some(&held) {
            seen.push(held);
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

/// One call a client makes to a child, and the state the composite's rule must then put it in.
struct Drive {
    /// The child this call forces.
    child: &'static str,
    /// The state it is forced into.
    state: &'static str,
    /// The state the rule then puts the composite in: awaited when the call COMPLETES the rule,
    /// and watched when it does not, because a rule that is not satisfied yet has to stay
    /// unsatisfied while the client goes on driving.
    composite: &'static str,
}

/// One row of the rule table: a rule, and the calls a client makes against it.
struct Row {
    /// What this row is, for every assertion message.
    case: &'static str,
    /// The `AlarmRule` the composite alarm is created with.
    rule: &'static str,
    /// The calls, in the order the client makes them.
    drives: &'static [Drive],
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_combined_rule_reaches_alarm_when_the_last_call_completes_it() {
    let rows: &[Row] = &[
        Row {
            case: "an AND rule over two children, completed by the second call",
            rule: "ALARM(\"kid-a\") AND ALARM(\"kid-b\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: OK },
                Drive { child: KID_B, state: ALARM, composite: ALARM },
            ],
        },
        Row {
            case: "the same AND rule with the children driven in the other order",
            rule: "ALARM(\"kid-a\") AND ALARM(\"kid-b\")",
            drives: &[
                Drive { child: KID_B, state: ALARM, composite: OK },
                Drive { child: KID_A, state: ALARM, composite: ALARM },
            ],
        },
        Row {
            case: "an AND rule over three children, completed by the third call",
            rule: "ALARM(\"kid-a\") AND ALARM(\"kid-b\") AND ALARM(\"kid-c\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: OK },
                Drive { child: KID_C, state: ALARM, composite: OK },
                Drive { child: KID_B, state: ALARM, composite: ALARM },
            ],
        },
        Row {
            case: "an AND rule one child leaving ALARM again un-completes",
            rule: "ALARM(\"kid-a\") AND ALARM(\"kid-b\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: OK },
                Drive { child: KID_B, state: ALARM, composite: ALARM },
                Drive { child: KID_A, state: OK, composite: OK },
            ],
        },
        Row {
            case: "an OR rule, which the first call completes",
            rule: "ALARM(\"kid-a\") OR ALARM(\"kid-b\")",
            drives: &[Drive { child: KID_B, state: ALARM, composite: ALARM }],
        },
        Row {
            case: "a parenthesised rule whose AND term is completed last",
            rule: "(ALARM(\"kid-a\") OR ALARM(\"kid-b\")) AND ALARM(\"kid-c\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: OK },
                Drive { child: KID_C, state: ALARM, composite: ALARM },
            ],
        },
        Row {
            case: "an AND rule whose second term is a negation the children can break",
            rule: "ALARM(\"kid-a\") AND NOT ALARM(\"kid-b\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: ALARM },
                Drive { child: KID_B, state: ALARM, composite: OK },
            ],
        },
        Row {
            case: "an AND rule combining two different state functions",
            rule: "ALARM(\"kid-a\") AND OK(\"kid-b\")",
            drives: &[
                Drive { child: KID_A, state: ALARM, composite: OK },
                Drive { child: KID_B, state: OK, composite: ALARM },
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        for child in [KID_A, KID_B, KID_C] {
            given_child(&router, child).await;
        }
        ok(
            &router,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": COMPOSITE,
                "AlarmRule": row.rule,
                "ActionsEnabled": false,
            }),
        )
        .await;
        // "When this operation creates an alarm, the alarm state is immediately set to
        // INSUFFICIENT_DATA. The alarm is then evaluated and its state is set appropriately."
        // (`smithy_query(query="operation",operation="PutCompositeAlarm")`) — no child is in
        // ALARM yet, so every rule above starts out unsatisfied.
        until_reports(&router, case, COMPOSITE, "StateValue", Some(OK)).await;

        for drive in row.drives {
            let (child, forced, wanted) = (drive.child, drive.state, drive.composite);
            force(&router, child, forced).await;
            if wanted == ALARM {
                until_reports(&router, case, COMPOSITE, "StateValue", Some(ALARM)).await;
            } else {
                // The rule is not satisfied by this call, and it must stay unsatisfied while the
                // client goes on driving — which is also what makes the NEXT call in this row a
                // call the earlier forced states are still standing for.
                let held = over_the_watch(&router, COMPOSITE, "StateValue").await;
                assert_eq!(
                    held,
                    vec![Some(wanted.to_string())],
                    "case {case:?}: forcing {child} to {forced} must leave the composite in \
                     {wanted} while the client drives on, got {held:?}"
                );
            }
        }
    }
}

/// One row of the suppression table: how the alarm is configured, and how the client drives its
/// suppressor.
struct Phase {
    /// What this row is, for every assertion message.
    case: &'static str,
    /// The alarm's `ActionsSuppressorExtensionPeriod`, in seconds.
    extension: i64,
    /// Whether the suppressor is driven into `ALARM` BEFORE the child completes the rule, which
    /// is the order that reaches the `Alarm` reason without passing through `WaitPeriod`.
    suppressor_first: bool,
    /// Whether this row's extension period is short enough to RUN OUT while it is watched, at
    /// which point the field stops being reported.
    runs_out: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_extension_period_is_reported_after_the_suppressor_leaves_alarm() {
    // The periods a row configures: one long enough that nothing it watches can run out while it
    // is watching, and one short enough for a row that waits for it to run out to see that
    // happen. Only the period a row TESTS is tight; every wait below is bounded by the generous
    // deadline instead.
    const LONG_PERIOD: i64 = 600;
    const SHORT_PERIOD: i64 = 3;

    let rows: &[Phase] = &[
        Phase {
            case: "the suppressor already in ALARM when the rule completes",
            extension: LONG_PERIOD,
            suppressor_first: true,
            runs_out: false,
        },
        Phase {
            case: "the suppressor confirming an alarm that is already in ALARM",
            extension: LONG_PERIOD,
            suppressor_first: false,
            runs_out: false,
        },
        Phase {
            case: "an extension period that runs out",
            extension: SHORT_PERIOD,
            suppressor_first: true,
            runs_out: true,
        },
        Phase {
            case: "no extension period at all",
            extension: 0,
            suppressor_first: true,
            runs_out: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        given_child(&router, KID_A).await;
        given_child(&router, SUPPRESSOR).await;
        ok(
            &router,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": COMPOSITE,
                "AlarmRule": format!("ALARM(\"{KID_A}\")"),
                "ActionsEnabled": false,
                "ActionsSuppressor": SUPPRESSOR,
                "ActionsSuppressorWaitPeriod": LONG_PERIOD,
                "ActionsSuppressorExtensionPeriod": row.extension,
            }),
        )
        .await;

        if row.suppressor_first {
            force(&router, SUPPRESSOR, ALARM).await;
            force(&router, KID_A, ALARM).await;
        } else {
            force(&router, KID_A, ALARM).await;
            force(&router, SUPPRESSOR, ALARM).await;
        }
        // "When the value is ALARM, it means that the actions are suppressed because the
        // suppressor alarm is in ALARM" — and the suppression does not change the alarm's own
        // state, which its rule decides.
        let suppressed = until_reports(
            &router,
            case,
            COMPOSITE,
            "ActionsSuppressedBy",
            Some("Alarm"),
        )
        .await;
        assert_eq!(
            suppressed["StateValue"].as_str(),
            Some(ALARM),
            "case {case:?}: the suppression must leave the state the rule computed, got \
             {suppressed}"
        );

        force(&router, SUPPRESSOR, OK).await;
        if row.runs_out {
            // "After this time, the composite alarm performs its actions." — the field is
            // reported only while a phase is in force, so a period of these seconds leaves it
            // unreported.
            let released =
                until_reports(&router, case, COMPOSITE, "ActionsSuppressedBy", None).await;
            assert_eq!(
                released["StateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: the period running out must not move the alarm its rule holds in \
                 ALARM, got {released}"
            );
        } else {
            let extended = until_reports(
                &router,
                case,
                COMPOSITE,
                "ActionsSuppressedBy",
                Some("ExtensionPeriod"),
            )
            .await;
            assert_eq!(
                extended["StateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: the extension period must withhold the actions without moving \
                 the alarm, got {extended}"
            );
            // "Captures the reason for action suppression."
            // (`smithy_query(query="doc",shape="CompositeAlarm",member="ActionsSuppressedReason"))`
            let reason = extended["ActionsSuppressedReason"]
                .as_str()
                .unwrap_or_default();
            assert!(
                reason.contains(SUPPRESSOR),
                "case {case:?}: the reason must name the alarm the composite is waiting on, got \
                 {reason:?}"
            );
            // A period that is still in force stays in force: the phase is keyed on the
            // configured period, not on how many times the alarm is read.
            let held = over_the_watch(&router, COMPOSITE, "ActionsSuppressedBy").await;
            assert_eq!(
                held,
                vec![Some("ExtensionPeriod".to_string())],
                "case {case:?}: the extension period must hold for the period it was configured \
                 with, got {held:?}"
            );
        }
    }
}
