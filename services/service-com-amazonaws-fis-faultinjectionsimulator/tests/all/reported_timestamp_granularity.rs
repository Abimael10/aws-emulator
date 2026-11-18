// SPDX-License-Identifier: CC0-1.0

//! How this service REPORTS the instants it holds, and the fixed sentences it reports for
//! a terminal status.
//!
//! RULE (the granularity): every timestamp member FIS publishes carries no
//! `@timestampFormat`, so `restJson1` writes it as an epoch-seconds NUMBER, and the
//! service reports those instants in whole seconds — the number a response carries never
//! has a sub-second part. The members are `ExperimentTemplate$creationTime`/
//! `lastUpdateTime` and the summary's copies of them, `Experiment$creationTime`/
//! `startTime`/`endTime` and the summary's copy, and `ExperimentAction$startTime`/
//! `endTime`
//! (`smithy_query(query="shape",shape="Experiment,ExperimentTemplate,ExperimentAction")`),
//! so [`TIMESTAMP_MEMBERS`] is that catalog and every response below is walked for ALL of
//! them wherever they appear — not only for the ones one operation is cited for.
//!
//! RULE (the terminal reason): `ExperimentState$reason` and `ExperimentActionState$reason`
//! are "the reason for the state", and the sentence FIS reports for a status it recognises
//! is FIXED text: a run that reached `completed` and an action the run carried out to the
//! end of its span each report that status's own whole sentence, with nothing of the
//! request or of the record substituted into it.
//!
//! The rows differ along what both rules generalise over: the operation the record is read
//! through (create, update, get, list, start, stop and a get of a run in each status it
//! passes through), how many actions the template carries, whether they name a `duration`
//! at all and whether they are chained through `startAfter`. Nothing here waits out a
//! modelled duration: the terminal status is polled for.

use std::time::Duration;

use serde_json::{json, Value};

use crate::experiment_lifecycle::{
    create_template, emulator, get_experiment, poll_until_final, poll_until_resolution_ends,
    send, start_experiment, stop_experiment, token, Answer, FALLBACK_REGION, RESOLUTION_BUDGET,
};

/// The timestamp members this service publishes, from the model's own shapes (see the
/// suite docs). A response is walked for every one of them, at any depth.
const TIMESTAMP_MEMBERS: [&str; 4] = ["creationTime", "lastUpdateTime", "startTime", "endTime"];

/// The whole sentence FIS reports as `ExperimentState$reason` for a run that reached the
/// terminal `completed` status.
const COMPLETED_REASON: &str = "Experiment completed.";

/// The whole sentence FIS reports as `ExperimentActionState$reason` for an action the run
/// carried out to the end of its own span.
const ACTION_COMPLETED_REASON: &str = "Action was completed.";

/// How long a row may take to reach its terminal status. The wait ends on the CONDITION
/// (`poll_until_final`), so this only has to be generous enough for a loaded host; the
/// emulator's own compressed run is a fraction of it.
const TERMINAL_BUDGET: Duration = Duration::from_secs(60);

/// The earliest instant a reported stamp can plausibly be: a stamp taken off the clock is
/// far past this, while a zero, an absent value or a constant would not be. Chosen well in
/// the past (2020-09-13) so the suite never ages out.
const PLAUSIBLE_FLOOR: f64 = 1_600_000_000.0;

/// A role ARN the emulator's IAM store holds no role for, so nothing is shown about its
/// trust policy or its permissions and every row's run goes ahead.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-reported-time";

/// One row: a template whose actions differ along what the granularity rule generalises
/// over.
struct Row {
    case: &'static str,
    actions: Value,
    /// The actions the run carries out, which report a progress of their own.
    carried_out: &'static [&'static str],
}

/// The rows: one action that names a duration, two actions chained through `startAfter`,
/// and an action that names no duration at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one action naming a duration",
            actions: json!({
                "pause": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT2M" } }
            }),
            carried_out: &["pause"],
        },
        Row {
            case: "two actions chained through startAfter",
            actions: json!({
                "first": { "actionId": "aws:fis:wait", "parameters": { "duration": "PT1M" } },
                "second": {
                    "actionId": "aws:fis:wait",
                    "parameters": { "duration": "PT1M" },
                    "startAfter": ["first"]
                }
            }),
            carried_out: &["first", "second"],
        },
        Row {
            // An action that names no `duration` at all, so the run is the compressed
            // floor: the stamps are reported the same way whatever span the plan has.
            case: "an action naming no duration",
            actions: json!({
                "assert": {
                    "actionId": "aws:cloudwatch:assert-alarm-state",
                    "parameters": {
                        "alarmArns": "arn:aws:cloudwatch:us-east-1:000000000000:alarm:cpu-high",
                        "alarmStates": "INSUFFICIENT_DATA"
                    }
                }
            }),
            carried_out: &["assert"],
        },
    ]
}

/// A `clientToken` seed from `label`: the model bounds `ClientToken` with
/// `pattern: ^[\S]+$`, so a case's own words are joined rather than spaced.
fn seed(label: &str) -> String {
    token(&label.replace(' ', "-"))
}

/// The create body for `row`.
fn template_body(row: &Row) -> Value {
    json!({
        "clientToken": seed(&format!("reported-time-{}", row.case)),
        "description": format!("reported timestamps: {}", row.case),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": {},
        "actions": row.actions.clone()
    })
}

/// Assert that every timestamp member anywhere in `body` is reported in whole seconds, and
/// answer how many were found.
///
/// A JSON number with a fractional part parses as a float, so the integer test is exactly
/// the wire test: a body carrying `1794227006.2567885` fails here and one carrying
/// `1794227006` passes.
fn assert_whole_second_stamps(case: &str, path: &str, body: &Value) -> usize {
    match body {
        Value::Object(members) => members
            .iter()
            .map(|(name, value)| {
                let nested = format!("{path}.{name}");
                if TIMESTAMP_MEMBERS.contains(&name.as_str()) && !value.is_null() {
                    assert!(
                        value.is_i64() || value.is_u64(),
                        "case {case:?}: expected {nested} to be an epoch-seconds number with no \
                         sub-second part, got {value:?}"
                    );
                    let seconds = value.as_f64().unwrap_or_default();
                    assert!(
                        seconds >= PLAUSIBLE_FLOOR,
                        "case {case:?}: expected {nested} to be an instant off the clock, got \
                         {seconds}"
                    );
                    return 1;
                }
                assert_whole_second_stamps(case, &nested, value)
            })
            .sum(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| assert_whole_second_stamps(case, &format!("{path}[{index}]"), item))
            .sum(),
        _ => 0,
    }
}

/// Assert that `answer` is a 200 whose every timestamp is reported in whole seconds, and
/// that it carries at least `at_least` of them — so a response that reports NO timestamp
/// cannot pass the rule by default.
fn assert_answer_stamps(case: &str, where_from: &str, answer: &Answer, at_least: usize) {
    assert_eq!(
        answer.status, 200,
        "case {case:?} ({where_from}): expected HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
    let found = assert_whole_second_stamps(&format!("{case} ({where_from})"), "$", &answer.body);
    assert!(
        found >= at_least,
        "case {case:?} ({where_from}): expected at least {at_least} reported timestamps, found \
         {found} in {:?}",
        answer.body
    );
}

/// `member` of `record` as epoch seconds.
fn stamp(case: &str, where_from: &str, record: &Value, member: &str) -> f64 {
    record[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?} ({where_from}): expected a {member} on {record:?}")
    })
}

#[tokio::test]
async fn every_reported_timestamp_is_an_epoch_seconds_number_with_no_sub_second_part() {
    let router = emulator();
    let rows = rows();

    // Every row is started first, so the runs overlap instead of being waited out one
    // after another; the account's active-experiment quota is five and there are fewer
    // rows than that.
    let mut started: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let case = row.case;
        let created = create_template(&router, template_body(row)).await;
        assert_answer_stamps(case, "CreateExperimentTemplate", &created, 2);
        let template = created.member("experimentTemplate").clone();
        let template_id = template["id"].as_str().unwrap_or_default().to_string();
        assert!(
            stamp(case, "CreateExperimentTemplate", &template, "lastUpdateTime")
                >= stamp(case, "CreateExperimentTemplate", &template, "creationTime"),
            "case {case:?}: lastUpdateTime must not precede creationTime, got {template:?}"
        );

        // An UPDATE of the same record reports the same members, and a GET of it reports
        // what the store holds: the rule is the record's, not one handler's.
        let updated = send(
            &router,
            "PATCH",
            &format!("/experimentTemplates/{template_id}"),
            Some(json!({ "description": format!("updated: {case}") })),
            FALLBACK_REGION,
        )
        .await;
        assert_answer_stamps(case, "UpdateExperimentTemplate", &updated, 2);
        let read = send(
            &router,
            "GET",
            &format!("/experimentTemplates/{template_id}"),
            None,
            FALLBACK_REGION,
        )
        .await;
        assert_answer_stamps(case, "GetExperimentTemplate", &read, 2);

        let accepted = start_experiment(
            &router,
            json!({
                "clientToken": seed(&format!("reported-time-start-{case}")),
                "experimentTemplateId": template_id
            }),
        )
        .await;
        assert_answer_stamps(case, "StartExperiment", &accepted, 2);
        let experiment_id = accepted.member("experiment")["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        started.push((row, experiment_id));
    }

    // The listers report the same records as summaries, which carry copies of the same
    // members.
    let listed_templates = send(
        &router,
        "GET",
        "/experimentTemplates",
        None,
        FALLBACK_REGION,
    )
    .await;
    assert_answer_stamps("every row", "ListExperimentTemplates", &listed_templates, 2);
    let listed_experiments = send(&router, "GET", "/experiments", None, FALLBACK_REGION).await;
    assert_answer_stamps("every row", "ListExperiments", &listed_experiments, 1);

    for (row, id) in &started {
        let case = row.case;
        // A run that is past starting up, read while it is still going: the stamps it has
        // so far are reported the same way, and the run's own window is not over.
        poll_until_resolution_ends(&router, id, RESOLUTION_BUDGET).await;
        let running = get_experiment(&router, id).await;
        assert_answer_stamps(case, "GetExperiment while the run is live", &running, 2);

        let ended = poll_until_final(&router, id, TERMINAL_BUDGET).await;
        let ended_answer = Answer {
            status: 200,
            body: json!({ "experiment": ended.clone() }),
        };
        // The terminal read: the experiment's three stamps plus each carried-out action's
        // two.
        let at_least = 3 + 2 * row.carried_out.len();
        assert_answer_stamps(case, "GetExperiment once terminal", &ended_answer, at_least);

        // The record's own ordering survives the projection: the run began no earlier than
        // the experiment was created and ended no earlier than it began, and every action
        // ran inside the window the run reports.
        let where_from = "GetExperiment once terminal";
        let created_at = stamp(case, where_from, &ended, "creationTime");
        let began = stamp(case, where_from, &ended, "startTime");
        let finished = stamp(case, where_from, &ended, "endTime");
        assert!(
            began >= created_at && finished >= began,
            "case {case:?}: expected creationTime <= startTime <= endTime, got \
             {created_at}, {began}, {finished}"
        );
        for name in row.carried_out {
            let action = ended["actions"][*name].clone();
            let action_began = stamp(case, where_from, &action, "startTime");
            let action_ended = stamp(case, where_from, &action, "endTime");
            assert!(
                action_began >= began && action_ended <= finished && action_ended >= action_began,
                "case {case:?}: expected action {name:?} to run inside the run's window \
                 [{began}, {finished}], got [{action_began}, {action_ended}]"
            );
        }
    }
}

#[tokio::test]
async fn a_stopped_run_reports_its_stop_stamps_in_whole_seconds_too() {
    let router = emulator();
    let case = "a run a stop ended";
    let created = create_template(&router, template_body(&rows()[0])).await;
    assert_answer_stamps(case, "CreateExperimentTemplate", &created, 2);
    let template_id = created.member("experimentTemplate")["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let accepted = start_experiment(
        &router,
        json!({
            "clientToken": token("reported-time-stop"),
            "experimentTemplateId": template_id
        }),
    )
    .await;
    assert_answer_stamps(case, "StartExperiment", &accepted, 2);
    let id = accepted.member("experiment")["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let stopped = stop_experiment(&router, &id).await;
    assert_answer_stamps(case, "StopExperiment", &stopped, 2);
    let settled = poll_until_final(&router, &id, TERMINAL_BUDGET).await;
    let settled_answer = Answer {
        status: 200,
        body: json!({ "experiment": settled.clone() }),
    };
    assert_answer_stamps(case, "GetExperiment once stopped", &settled_answer, 3);
}

#[tokio::test]
async fn a_completed_run_and_its_actions_report_the_fixed_sentences_for_that_status() {
    let router = emulator();
    let rows = rows();

    let mut started: Vec<(&Row, String, String)> = Vec::new();
    for row in &rows {
        let case = row.case;
        let created = create_template(&router, template_body(row)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be created, got {} with {:?}",
            created.status, created.body
        );
        let template_id = created.member("experimentTemplate")["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let accepted = start_experiment(
            &router,
            json!({
                "clientToken": seed(&format!("reason-start-{case}")),
                "experimentTemplateId": template_id
            }),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected the start to be accepted, got {} with {:?}",
            accepted.status, accepted.body
        );
        let id = accepted.member("experiment")["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        started.push((row, template_id, id));
    }

    for (row, template_id, id) in &started {
        let case = row.case;
        let ended = poll_until_final(&router, id, TERMINAL_BUDGET).await;
        assert_eq!(
            ended["state"]["status"].as_str().unwrap_or_default(),
            "completed",
            "case {case:?}: expected the run to complete, got {ended:?}"
        );
        let reason = ended["state"]["reason"].as_str().unwrap_or_default();
        assert_eq!(
            reason, COMPLETED_REASON,
            "case {case:?}: expected the completed run's reason to be {COMPLETED_REASON:?}, \
             got {reason:?}"
        );
        for name in row.carried_out {
            let action_reason = ended["actions"][*name]["state"]["reason"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                ended["actions"][*name]["state"]["status"]
                    .as_str()
                    .unwrap_or_default(),
                "completed",
                "case {case:?}: expected action {name:?} to have been carried out, got {:?}",
                ended["actions"][*name]
            );
            assert_eq!(
                action_reason, ACTION_COMPLETED_REASON,
                "case {case:?}: expected the completed action {name:?} to report \
                 {ACTION_COMPLETED_REASON:?}, got {action_reason:?}"
            );
        }
        // Fixed text: no value of the request or of the record is substituted into either
        // sentence, so the same string is reported for every run and every action.
        let mut named: Vec<String> = vec![
            id.clone(),
            template_id.clone(),
            ROLE_ARN.to_string(),
            case.to_string(),
        ];
        named.extend(row.carried_out.iter().map(|name| (*name).to_string()));
        let action_reasons: Vec<String> = row
            .carried_out
            .iter()
            .map(|name| {
                ended["actions"][*name]["state"]["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        for value in &named {
            assert!(
                !reason.contains(value.as_str()),
                "case {case:?}: a state reason names nothing of the request, got {reason:?} \
                 carrying {value:?}"
            );
            for action_reason in &action_reasons {
                assert!(
                    !action_reason.contains(value.as_str()),
                    "case {case:?}: an action state reason names nothing of the request, got \
                     {action_reason:?} carrying {value:?}"
                );
            }
        }
    }
}

/// The reasons are the SERVICE's, not one row's: two runs of two different templates
/// report the same sentence for the same status.
#[tokio::test]
async fn the_terminal_reason_is_the_same_sentence_for_every_run() {
    let router = emulator();
    let mut reported: Vec<(&str, String)> = Vec::new();
    for row in &rows()[..2] {
        let case = row.case;
        let created = create_template(
            &router,
            json!({
                "clientToken": seed(&format!("shared-reason-{case}")),
                "description": format!("one sentence per status: {case}"),
                "roleArn": ROLE_ARN,
                "stopConditions": [{ "source": "none" }],
                "targets": {},
                "actions": row.actions.clone()
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be created, got {} with {:?}",
            created.status, created.body
        );
        let template_id = created.member("experimentTemplate")["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let accepted = start_experiment(
            &router,
            json!({
                "clientToken": seed(&format!("shared-reason-start-{case}")),
                "experimentTemplateId": template_id
            }),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected the start to be accepted, got {} with {:?}",
            accepted.status, accepted.body
        );
        reported.push((
            case,
            accepted.member("experiment")["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    let mut sentences: Vec<String> = Vec::new();
    for (case, id) in &reported {
        let ended = poll_until_final(&router, id, TERMINAL_BUDGET).await;
        sentences.push(
            ended["state"]["reason"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no state reason on {ended:?}"))
                .to_string(),
        );
    }
    assert!(
        sentences.windows(2).all(|pair| pair[0] == pair[1]),
        "expected one sentence for the completed status across the rows, got {sentences:?}"
    );
}
