// SPDX-License-Identifier: CC0-1.0

//! RULE: the account's safety lever refuses no `StartExperiment`. A start made while the
//! lever is holding experiments back is ACCEPTED, and the lever is enforced on the RUN
//! instead: the experiment it created never runs, it settles to the modelled terminal
//! status `cancelled` under a `state.reason` naming that experiment and the lever's ARN,
//! and every action of its plan reports `cancelled` having never started. Once the lever is
//! disengaged the same start runs to `completed`.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rows differ along what the rule generalises over: WHICH held status the lever is in
//! when the start arrives (the `engaging` an engage reports at once, and the `engaged` the
//! clock settles it to), the Region the start is signed for (which is the Region the lever
//! ARN in the reason names), HOW MANY actions the cancelled run's plan holds and whether
//! they wait for one another, and the `duration` those actions name — including the longest
//! `aws:fis:wait` FIS documents (720 minutes), so a run that would have taken the longest is
//! cancelled just the same.
//!
//! Every wait here ends on a CONDITION (the lever reporting the status asked for, the
//! experiment reaching a terminal status), polled until a generous deadline, so no case
//! depends on how fast the host runs.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{emulator, poll_until_final, send, token};

/// The account id every ARN this emulator mints carries (`::engine::EMULATOR_ACCOUNT_ID`),
/// spelled out so a changed account fails here.
const ACCOUNT_ID: &str = "000000000000";

/// The id the account's one safety lever is named by: no API creates or enumerates a lever,
/// so it already exists under this id.
const LEVER_ID: &str = "default";

/// The status an experiment the lever cancelled reports — one of `ExperimentStatus`'s
/// modelled values (`smithy_query(query="shape",shape="ExperimentStatus")`), written out
/// here rather than imported from the crate so a wrong value in the handler fails this
/// suite.
const CANCELLED: &str = "cancelled";

/// The status an experiment whose actions ran to the end reports. See [`CANCELLED`].
const COMPLETED: &str = "completed";

/// The status a freshly accepted start reports, before the clock has moved the run on.
const INITIATING: &str = "initiating";

/// The status an action of a cancelled run reports — one of `ExperimentActionStatus`'s
/// modelled values (`smithy_query(query="shape",shape="ExperimentActionStatus")`).
const ACTION_CANCELLED: &str = "cancelled";

/// How long a case gives a condition it polls for: generous, because the wait ends as soon
/// as the condition holds.
const BUDGET: Duration = Duration::from_secs(60);

/// How often a poll looks again.
const INTERVAL: Duration = Duration::from_millis(25);

/// One action of a row's template: its name, the ISO 8601 `duration` it asks for — FIS
/// documents `duration` as required for `aws:fis:wait`, inside 1 to 720 minutes — and the
/// action it waits for.
#[derive(Debug, Clone, Copy)]
struct Action {
    name: &'static str,
    duration: &'static str,
    start_after: Option<&'static str>,
}

/// One case of the rule. See the suite docs for what the rows span.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The Region the calls of this row are signed for, and so the Region the lever ARN in
    /// the cancellation reason names.
    region: &'static str,
    /// Whether the start is made before the engage has taken effect — the lever reporting
    /// the transient `engaging` — rather than after the clock has settled it to `engaged`.
    start_while_the_engage_is_taking_effect: bool,
    actions: &'static [Action],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "engaged-one-targetless-wait",
            region: "us-east-1",
            start_while_the_engage_is_taking_effect: false,
            actions: &[Action {
                name: "waitAction",
                duration: "PT1M",
                start_after: None,
            }],
        },
        Row {
            case: "engaging-one-targetless-wait",
            region: "eu-west-1",
            start_while_the_engage_is_taking_effect: true,
            actions: &[Action {
                name: "waitAction",
                duration: "PT1M",
                start_after: None,
            }],
        },
        Row {
            case: "engaged-three-actions-in-order",
            region: "ap-southeast-3",
            start_while_the_engage_is_taking_effect: false,
            actions: &[
                Action {
                    name: "firstWait",
                    duration: "PT2M",
                    start_after: None,
                },
                Action {
                    name: "secondWait",
                    duration: "PT3M",
                    start_after: Some("firstWait"),
                },
                Action {
                    name: "thirdWait",
                    duration: "PT1M",
                    start_after: Some("secondWait"),
                },
            ],
        },
        Row {
            // The longest `duration` FIS documents for `aws:fis:wait` (1 to 720 minutes):
            // the run that would have taken the longest is cancelled like any other.
            case: "engaged-longest-documented-wait",
            region: "sa-east-1",
            start_while_the_engage_is_taking_effect: false,
            actions: &[Action {
                name: "waitAction",
                duration: "PT720M",
                start_after: None,
            }],
        },
    ]
}

/// The `actions` map `row`'s template declares.
fn actions_of(row: &Row) -> Value {
    let mut actions = serde_json::Map::new();
    for action in row.actions {
        let mut declared = serde_json::Map::new();
        declared.insert("actionId".to_string(), json!("aws:fis:wait"));
        declared.insert(
            "parameters".to_string(),
            json!({ "duration": action.duration }),
        );
        if let Some(earlier) = action.start_after {
            declared.insert("startAfter".to_string(), json!([earlier]));
        }
        actions.insert(action.name.to_string(), Value::Object(declared));
    }
    Value::Object(actions)
}

/// Create `row`'s template — targetless actions, so the run depends on no other service's
/// resources — and report its id.
async fn given_template(router: &MultiServiceRouter, row: &Row) -> String {
    let created = send(
        router,
        "POST",
        "/experimentTemplates",
        Some(json!({
            "clientToken": token(&format!("lever-cancel-template-{}", row.case)),
            "description": format!("a run the safety lever cancels: {}", row.case),
            "roleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/fis-lever-cancel"),
            "stopConditions": [{ "source": "none" }],
            "targets": {},
            "actions": actions_of(row)
        })),
        row.region,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected HTTP 200 from CreateExperimentTemplate, got {} with {:?}",
        row.case, created.status, created.body
    );
    created.member("experimentTemplate")["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: no experimentTemplate.id in {:?}",
                row.case, created.body
            )
        })
        .to_string()
}

/// Ask the account's lever for `status` under `reason`, and report what the update answered.
async fn update_lever(
    router: &MultiServiceRouter,
    region: &str,
    status: &str,
    reason: &str,
) -> Value {
    let answer = send(
        router,
        "PATCH",
        &format!("/safetyLevers/{LEVER_ID}/state"),
        Some(json!({ "state": { "status": status, "reason": reason } })),
        region,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "expected HTTP 200 from UpdateSafetyLeverState, got {} with {:?}",
        answer.status, answer.body
    );
    answer.member("safetyLever").clone()
}

/// The lever as `GetSafetyLever` reports it.
async fn read_lever(router: &MultiServiceRouter, region: &str) -> Value {
    let answer = send(
        router,
        "GET",
        &format!("/safetyLevers/{LEVER_ID}"),
        None,
        region,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "expected HTTP 200 from GetSafetyLever, got {} with {:?}",
        answer.status, answer.body
    );
    answer.member("safetyLever").clone()
}

/// The `state.status` a reported lever or experiment carries.
fn status_of(reported: &Value) -> String {
    reported["state"]["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll `GetSafetyLever` until the lever reports `status`, or the budget runs out.
async fn poll_lever_until(router: &MultiServiceRouter, region: &str, status: &str) -> Value {
    let deadline = std::time::Instant::now() + BUDGET;
    loop {
        let lever = read_lever(router, region).await;
        if status_of(&lever) == status || std::time::Instant::now() >= deadline {
            return lever;
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

/// Start an experiment from `template_id`, signed for `region`.
async fn start(
    router: &MultiServiceRouter,
    region: &str,
    label: &str,
    template_id: &str,
) -> (u16, Value) {
    let answer = send(
        router,
        "POST",
        "/experiments",
        Some(json!({
            "clientToken": token(&format!("lever-cancel-start-{label}")),
            "experimentTemplateId": template_id
        })),
        region,
    )
    .await;
    (answer.status, answer.body.clone())
}

/// The `state.reason` FIS gives an experiment its safety lever cancelled: the experiment and
/// the lever that cancelled it. Spelled out here rather than imported from the crate, so
/// changed wording fails this suite.
fn cancellation_reason(experiment_id: &str, region: &str) -> String {
    format!(
        "Experiment {experiment_id} has been cancelled by the safety lever \
         'arn:aws:fis:{region}:{ACCOUNT_ID}:safety-lever/{LEVER_ID}'."
    )
}

#[tokio::test]
async fn a_start_made_while_the_lever_holds_is_accepted_and_the_run_is_cancelled() {
    for row in rows() {
        // A FRESH emulator per row: the lever is an account-level record, so one row's
        // engage must not decide another row's start.
        let router = emulator();
        let template_id = given_template(&router, &row).await;

        let engaged = update_lever(&router, row.region, "engaged", "hold every experiment").await;
        assert_eq!(
            status_of(&engaged),
            "engaging",
            "case {:?}: the engage is taking effect, got {engaged:?}",
            row.case
        );
        let holding = if row.start_while_the_engage_is_taking_effect {
            engaged
        } else {
            poll_lever_until(&router, row.region, "engaged").await
        };
        let held = status_of(&holding);
        assert!(
            ["engaging", "engaged"].contains(&held.as_str()),
            "case {:?}: expected the lever to be holding experiments back, got {held:?} in \
             {holding:?}",
            row.case
        );

        // The start is ACCEPTED: the lever refuses nothing.
        let (status, body) = start(&router, row.region, row.case, &template_id).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from a start made while the lever is {held:?}, got \
             {status} with {body:?}",
            row.case
        );
        let experiment = body["experiment"].clone();
        let experiment_id = experiment["id"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the accepted start reports an experiment id, got {body:?}",
                    row.case
                )
            })
            .to_string();
        assert_eq!(
            status_of(&experiment),
            INITIATING,
            "case {:?}: the accepted start reports the experiment it created, got {experiment:?}",
            row.case
        );

        // And the lever is enforced on the RUN: it settles cancelled, naming itself and the
        // lever that cancelled it.
        let settled = poll_until_final(&router, &experiment_id, BUDGET).await;
        assert_eq!(
            status_of(&settled),
            CANCELLED,
            "case {:?}: expected the run to settle {CANCELLED:?}, got {settled:?}",
            row.case
        );
        assert_eq!(
            settled["state"]["reason"].as_str().unwrap_or_default(),
            cancellation_reason(&experiment_id, row.region),
            "case {:?}: expected the safety-lever cancellation reason, got {settled:?}",
            row.case
        );
        assert!(
            settled["state"]["error"].is_null(),
            "case {:?}: a cancelled run reports no action error, got {settled:?}",
            row.case
        );
        assert!(
            !settled["endTime"].is_null(),
            "case {:?}: a cancelled run carries the endTime it ended at, got {settled:?}",
            row.case
        );

        // Every action of the run is cancelled, and none of them ever ran.
        for action in row.actions {
            let reported = &settled["actions"][action.name];
            assert_eq!(
                status_of(reported),
                ACTION_CANCELLED,
                "case {:?}: expected action {:?} to report {ACTION_CANCELLED:?}, got {reported:?}",
                row.case,
                action.name
            );
            assert!(
                reported["startTime"].is_null() && reported["endTime"].is_null(),
                "case {:?}: action {:?} never ran, so it carries no start or end time, got \
                 {reported:?}",
                row.case,
                action.name
            );
        }

        // Nothing of the cancellation is a refusal: the experiment is a record every later
        // read reports, under the id the start handed back.
        let listed = send(
            &router,
            "GET",
            "/experiments",
            None,
            row.region,
        )
        .await;
        let summaries = listed.member("experiments").clone();
        let summary = summaries
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .find(|summary| summary["id"].as_str() == Some(experiment_id.as_str()))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: ListExperiments must report the cancelled experiment, got \
                     {summaries:?}",
                    row.case
                )
            });
        assert_eq!(
            status_of(&summary),
            CANCELLED,
            "case {:?}: the listed summary agrees with the experiment, got {summary:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_same_start_runs_to_completed_once_the_lever_is_disengaged() {
    // The rows differ along the lever's history before the start: a lever nothing ever
    // engaged, one engaged and then disengaged while the engage was still taking effect, and
    // one disengaged after it had settled to `engaged`.
    for (case, engage, settle_first) in [
        ("never-engaged", false, false),
        ("disengaged-while-engaging", true, false),
        ("disengaged-after-engaged", true, true),
    ] {
        let router = emulator();
        let row = Row {
            case,
            region: "us-east-1",
            start_while_the_engage_is_taking_effect: false,
            actions: &[Action {
                name: "waitAction",
                duration: "PT1M",
                start_after: None,
            }],
        };
        let template_id = given_template(&router, &row).await;

        if engage {
            update_lever(&router, row.region, "engaged", "hold them for now").await;
            if settle_first {
                poll_lever_until(&router, row.region, "engaged").await;
            }
            let disengaged =
                update_lever(&router, row.region, "disengaged", "experiments may run").await;
            assert_eq!(
                status_of(&disengaged),
                "disengaged",
                "case {case:?}: a disengage lands at once, got {disengaged:?}"
            );
        }

        let (status, body) = start(&router, row.region, case, &template_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from a start made with the lever disengaged, got \
             {status} with {body:?}"
        );
        let experiment_id = body["experiment"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no experiment id in {body:?}"))
            .to_string();

        let settled = poll_until_final(&router, &experiment_id, BUDGET).await;
        assert_eq!(
            status_of(&settled),
            COMPLETED,
            "case {case:?}: expected the run to reach {COMPLETED:?}, got {settled:?}"
        );
        let action = &settled["actions"]["waitAction"];
        assert_eq!(
            status_of(action),
            COMPLETED,
            "case {case:?}: expected the action of a completed run to report \
             {COMPLETED:?}, got {action:?}"
        );
    }
}
