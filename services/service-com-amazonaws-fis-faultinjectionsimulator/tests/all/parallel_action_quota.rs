// SPDX-License-Identifier: CC0-1.0

//! RULE: an experiment template may declare no more actions that START TOGETHER than the
//! account's parallel-action quota allows — and an action that declares `startAfter` waits
//! for another action to complete, so it is sequenced and is not one of them. A template
//! past that quota is refused with the quota's own error class
//! (`ServiceQuotaExceededException`, HTTP 402) and nothing is created, while a template that
//! carries MORE actions than the quota with the surplus sequenced is admitted.
//!
//! The rule is a property of the TEMPLATE rather than of the call that built it, so
//! `CreateExperimentTemplate` and `UpdateExperimentTemplate` judge the same action map alike,
//! down to the message.
//!
//! The rows differ along what the rule generalises over: how many actions start together
//! (one, one below the quota, the quota itself, one past it), how many actions the template
//! carries in all, how those actions are sequenced (one chain, several chains, every action
//! behind the first), and the FORM the absence of a sequencing takes (no `startAfter` member
//! at all, or one naming no earlier action).
//!
//! The HTTP harness is the one `tests/all/experiment_lifecycle.rs` sets up, as each sibling
//! suite's is.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{
    create_template, emulator, send, token, Answer, FALLBACK_REGION,
};

/// A well-formed IAM role ARN: every row here is about the actions, not about the role.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-parallel-actions";

/// FIS's built-in wait action, and a `duration` inside the 1..720 minutes documented for it
/// — the action every row declares, so a row's verdict is about how many of them start
/// together and nothing else.
const ACTION_ID: &str = "aws:fis:wait";
/// See [`ACTION_ID`].
const WAIT_DURATION: &str = "PT1M";

/// How many of a template's actions may start together. Spelled out here rather than
/// imported from the crate, so a changed cap in `src/state.rs` fails this suite.
const PARALLEL_QUOTA: usize = 10;

/// How many actions one template may carry in all ("Actions per experiment template"), the
/// other quota the action map is judged by: every row stays within it, so a row's verdict is
/// the parallel quota's own.
const ACTIONS_QUOTA: usize = 20;

/// The refusal a template past the parallel quota earns. The whole sentence is fixed text;
/// the only value in it is the quota itself ([`PARALLEL_QUOTA`]).
fn expected_message() -> String {
    format!("The number of parallel actions cannot exceed {PARALLEL_QUOTA}")
}

/// How an action that waits for nothing says so.
#[derive(Debug, Clone, Copy)]
enum Unsequenced {
    /// It declares no `startAfter` member at all.
    Absent,
    /// It declares an empty `startAfter` list, which names no earlier action — the same
    /// reading of "not specified" an empty collection gets elsewhere in a template.
    Empty,
}

/// One template shape: its actions as a set of chains, and whether the sequence admits it.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// One entry per chain, holding how many actions that chain is long. The FIRST action of
    /// each chain waits for nothing, so the number of chains is how many actions start
    /// together; every later action of a chain declares `startAfter` on the one before it.
    chains: &'static [usize],
    /// How the first action of each chain says it waits for nothing.
    unsequenced: Unsequenced,
    /// `true` for a template the parallel quota refuses.
    refused: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single action",
            chains: &[1],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "one action below the parallel quota",
            chains: &[1, 1, 1, 1, 1, 1, 1, 1, 1],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "exactly the parallel quota",
            chains: &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "one action past the parallel quota",
            chains: &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            unsequenced: Unsequenced::Absent,
            refused: true,
        },
        Row {
            case: "one action past the parallel quota, each naming no earlier action",
            chains: &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            unsequenced: Unsequenced::Empty,
            refused: true,
        },
        Row {
            case: "one action more than the parallel quota, the surplus sequenced",
            chains: &[2, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "the whole action quota in one chain",
            chains: &[ACTIONS_QUOTA],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "the whole action quota behind the first action",
            chains: &[ACTIONS_QUOTA],
            unsequenced: Unsequenced::Empty,
            refused: false,
        },
        Row {
            case: "pairs of actions, each pair starting together with the others",
            chains: &[2, 2, 2, 2, 2, 2],
            unsequenced: Unsequenced::Absent,
            refused: false,
        },
        Row {
            case: "one chain past the parallel quota among sequenced actions",
            chains: &[2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1],
            unsequenced: Unsequenced::Absent,
            refused: true,
        },
    ]
}

/// The action map `row` describes.
fn actions(row: &Row) -> Value {
    let mut map = Map::new();
    for (chain, length) in row.chains.iter().enumerate() {
        for step in 0..*length {
            let mut action = json!({
                "actionId": ACTION_ID,
                "parameters": { "duration": WAIT_DURATION }
            });
            if step == 0 {
                if let Unsequenced::Empty = row.unsequenced {
                    action["startAfter"] = json!([]);
                }
            } else {
                action["startAfter"] = json!([format!("wait-{chain}-{}", step - 1)]);
            }
            map.insert(format!("wait-{chain}-{step}"), action);
        }
    }
    Value::Object(map)
}

/// How many actions `row` declares in all, and how many of them start together.
fn counts(row: &Row) -> (usize, usize) {
    (row.chains.iter().sum(), row.chains.len())
}

/// The create body for `row`.
fn create_body(row: &Row, label: &str) -> Value {
    json!({
        "clientToken": token(&format!("{label}-{}", slug(row.case))),
        "description": format!("parallel action quota: {}", row.case),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": {},
        "actions": actions(row)
    })
}

/// A template every update row starts from: one action, so the baseline itself is well
/// inside both quotas.
fn baseline_body(label: &str) -> Value {
    json!({
        "clientToken": token(&format!("baseline-{label}")),
        "description": format!("parallel action quota baseline: {label}"),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": {},
        "actions": {
            "wait": { "actionId": ACTION_ID, "parameters": { "duration": WAIT_DURATION } }
        }
    })
}

/// `case` as a token-safe label (`ClientToken` is `pattern ^[\S]+$`).
fn slug(case: &str) -> String {
    case.replace(' ', "-")
}

/// The `message` an answer carries.
fn message(answer: &Answer) -> String {
    answer.body["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `UpdateExperimentTemplate`: replace the stored template's action map with `row`'s.
async fn update_template(router: &MultiServiceRouter, id: &str, body: Value) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/experimentTemplates/{id}"),
        Some(body),
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

/// The ids `ListExperimentTemplates` reports.
async fn listed_templates(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let answer = send(router, "GET", "/experimentTemplates", None, FALLBACK_REGION).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from ListExperimentTemplates, got {} with {:?}",
        answer.status, answer.body
    );
    answer
        .member("experimentTemplates")
        .as_array()
        .map(|templates| {
            templates
                .iter()
                .filter_map(|template| template["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Assert `answer` is the refusal the parallel quota answers with, or an accepted template
/// when `row` is one the quota admits. Returns the refusal as a string, so the two calls can
/// be compared with each other.
fn assert_verdict(row: &Row, call: &str, answer: &Answer) -> String {
    let (declared, parallel) = counts(row);
    if !row.refused {
        assert_eq!(
            answer.status, 200,
            "case {:?}: {parallel} of {declared} actions start together, which is within the \
             quota of {PARALLEL_QUOTA}, so {call} must accept it; got {} with {:?}",
            row.case, answer.status, answer.body
        );
        return String::new();
    }
    assert_eq!(
        answer.status, 402,
        "case {:?}: {parallel} of {declared} actions start together, past the quota of \
         {PARALLEL_QUOTA}, so {call} must refuse with HTTP 402; got {} with {:?}",
        row.case, answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ServiceQuotaExceededException",
        "case {:?}: expected ServiceQuotaExceededException from {call}, got {:?}",
        row.case,
        answer.body
    );
    assert_eq!(
        message(answer),
        expected_message(),
        "case {:?}: expected the quota's own wording from {call}, got {:?}",
        row.case,
        answer.body
    );
    format!("{} {} {}", answer.status, answer.error_code(), message(answer))
}

/// Every action the row declares is stored, with the sequencing it declared.
fn assert_actions_stored(row: &Row, stored: &Value) {
    let (declared, _) = counts(row);
    let expected = actions(row);
    let stored_actions = stored["actions"].as_object().unwrap_or_else(|| {
        panic!(
            "case {:?}: expected the stored template to carry an action map, got {stored:?}",
            row.case
        )
    });
    assert_eq!(
        stored_actions.len(),
        declared,
        "case {:?}: expected {declared} stored actions, got {stored_actions:?}",
        row.case
    );
    for (name, action) in expected.as_object().expect("the row's actions are a map") {
        let stored_action = &stored["actions"][name];
        assert_eq!(
            stored_action["actionId"].as_str(),
            action["actionId"].as_str(),
            "case {:?}: expected {name} to keep its actionId, got {stored_action:?}",
            row.case
        );
        // An action that names an earlier one keeps that sequencing; an empty list names
        // none, so what a template reports for it is not what this rule is about.
        let names_earlier = action["startAfter"]
            .as_array()
            .is_some_and(|earlier| !earlier.is_empty());
        if names_earlier {
            assert_eq!(
                stored_action["startAfter"], action["startAfter"],
                "case {:?}: expected {name} to keep its startAfter, got {stored_action:?}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn the_parallel_action_quota_counts_the_actions_that_start_together() {
    for row in rows() {
        let router = emulator();

        let created = create_template(&router, create_body(&row, "create")).await;
        let from_create = assert_verdict(&row, "CreateExperimentTemplate", &created);

        if row.refused {
            // A refused create leaves nothing behind: no template of this account exists.
            let listed = listed_templates(&router, row.case).await;
            assert!(
                listed.is_empty(),
                "case {:?}: a refused create must create nothing, got {listed:?}",
                row.case
            );
        } else {
            let template = created.member("experimentTemplate").clone();
            let id = template["id"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {:?}: no experimentTemplate.id in {template:?}", row.case)
                })
                .to_string();
            let stored = read_template(&router, row.case, &id).await;
            assert_eq!(
                stored, template,
                "case {:?}: GetExperimentTemplate must report what the create stored, got \
                 {stored:?}",
                row.case
            );
            assert_actions_stored(&row, &stored);
        }

        // The same action map, reached through the update instead: one rule judges a created
        // and an updated template alike.
        let baseline = create_template(&router, baseline_body(&slug(row.case))).await;
        assert_eq!(
            baseline.status, 200,
            "case {:?}: expected the baseline template to be created, got {} with {:?}",
            row.case, baseline.status, baseline.body
        );
        let before = baseline.member("experimentTemplate").clone();
        let id = before["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no experimentTemplate.id in {before:?}", row.case))
            .to_string();

        let updated = update_template(&router, &id, json!({ "actions": actions(&row) })).await;
        let from_update = assert_verdict(&row, "UpdateExperimentTemplate", &updated);
        assert_eq!(
            from_create, from_update,
            "case {:?}: both calls must judge the same action map alike, got {from_create:?} \
             from the create and {from_update:?} from the update",
            row.case
        );

        let after = read_template(&router, row.case, &id).await;
        if row.refused {
            assert_eq!(
                after, before,
                "case {:?}: a refused update must change nothing, got {after:?}",
                row.case
            );
        } else {
            assert_actions_stored(&row, &after);
        }
    }
}
