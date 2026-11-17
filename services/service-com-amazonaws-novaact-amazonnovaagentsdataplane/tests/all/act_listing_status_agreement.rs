// SPDX-License-Identifier: CC0-1.0

//! `ListActs`: the status each listed act is IN, element by element.
//!
//! `ActSummary$status` is "the current execution status of the act"
//! (`smithy_query(query="shape",shape="ActSummary")`), so the rule these cases assert is
//! that EVERY element of a page carries the status of the act that element names — the one
//! the operations that drive an act (its creation, its steps, its updates) last left that
//! act in — and that `endedAt` is reported exactly for an element whose status is one that
//! finishes an act. A listing therefore reports as many different statuses as the acts in
//! it hold, and a value that is the same for every element cannot satisfy it.
//!
//! So the table below varies what that rule generalises over: HOW each act of one session
//! was driven into the status it holds (created and left alone, stepped with a tool left to
//! run, stepped past its last tool, or updated to each of the remaining statuses) and
//! therefore WHICH of the statuses `ActStatus` declares it holds, with every declared value
//! present in one page. The cases then vary how that page is read: in one response, after
//! one act has moved, and split into pages in each sort order.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::ACT_STATUS_VALUES;

use crate::act_listing::{list, summaries, Query};
use crate::act_status_update::{
    chain, move_to, new_act, Chain, FINISHED_STATUSES, INITIAL_ACT_STATUS,
};
use crate::act_step_invocation::{act_declaring, step_with, tool_spec, unattributed_result};
use crate::workflow_lifecycle::{text, Emulator};

/// The definition each case builds its chain under.
const DEFINITION: &str = "act-listing-status";

/// How one act of the fixture is driven into the status it holds.
#[derive(Debug, Clone, Copy)]
enum Driven {
    /// Created and left alone, so the act holds what `CreateAct` answered for it.
    Created,
    /// One `InvokeActStep` on an act declaring this many tools: a step asks for the next
    /// tool the act declared, and an act with an outstanding call is waiting on its caller
    /// while one with no tool left to ask for has run to the end.
    Stepped { tools: usize },
    /// One `UpdateAct` naming this status.
    Updated(&'static str),
}

/// One act of the fixture: how it was driven, and the status it therefore holds.
#[derive(Debug, Clone)]
struct DrivenAct {
    label: String,
    act_id: String,
    status: String,
}

/// Whether `status` is one an act has FINISHED in, i.e. one that owes an `endedAt`.
fn finishes(status: &str) -> bool {
    FINISHED_STATUSES.contains(&status)
}

/// One act per row, driven into the status the row names, all under one session so one
/// listing reports them together.
async fn driven_acts(emulator: &Emulator, chain: &Chain) -> Vec<DrivenAct> {
    // Rows: the way the act is driven, and the status that leaves it in. Together they cover
    // every value `ActStatus` declares, which the case below checks against the model's own
    // value list rather than against this table.
    let rows: [(&str, Driven, &str); 8] = [
        (
            "created and left waiting on its client",
            Driven::Created,
            INITIAL_ACT_STATUS,
        ),
        (
            "stepped with a tool left to run",
            Driven::Stepped { tools: 1 },
            "PENDING_CLIENT_ACTION",
        ),
        (
            "stepped past its last declared tool",
            Driven::Stepped { tools: 0 },
            "SUCCEEDED",
        ),
        (
            "updated to running",
            Driven::Updated("RUNNING"),
            "RUNNING",
        ),
        (
            "updated to waiting on a human",
            Driven::Updated("PENDING_HUMAN_ACTION"),
            "PENDING_HUMAN_ACTION",
        ),
        ("updated to failed", Driven::Updated("FAILED"), "FAILED"),
        (
            "updated to timed out",
            Driven::Updated("TIMED_OUT"),
            "TIMED_OUT",
        ),
        (
            "updated to succeeded",
            Driven::Updated("SUCCEEDED"),
            "SUCCEEDED",
        ),
    ];

    let mut driven: Vec<DrivenAct> = Vec::new();
    for (label, how, status) in rows {
        let act_id = match how {
            Driven::Created => new_act(emulator, chain, label).await,
            Driven::Stepped { tools } => {
                let specs = (0..tools)
                    .map(|index| tool_spec(&format!("tool_{index}"), json!({ "type": "object" })))
                    .collect();
                let act_id = act_declaring(emulator, chain, label, specs).await;
                let (code, answer) =
                    step_with(emulator, chain, &act_id, vec![unattributed_result("go")]).await;
                assert_eq!(
                    code, 200,
                    "case {label:?}: the step must be accepted, got {code} {answer}"
                );
                // The step's own answer agrees with the status the act is left in: a step
                // that handed a call out left work with the caller, one that handed none out
                // had nothing left to ask for.
                let calls = answer["calls"]
                    .as_array()
                    .unwrap_or_else(|| panic!("case {label:?}: calls must be a list, got {answer}"))
                    .len();
                assert_eq!(
                    calls == 0,
                    finishes(status),
                    "case {label:?}: a step that asks for no call is the one that finishes \
                     the act, got {calls} call(s) for {status}"
                );
                act_id
            }
            Driven::Updated(requested) => {
                let act_id = new_act(emulator, chain, label).await;
                let (code, answer) = move_to(emulator, chain, &act_id, requested).await;
                assert_eq!(
                    code, 200,
                    "case {label:?}: the update must be accepted, got {code} {answer}"
                );
                act_id
            }
        };
        driven.push(DrivenAct {
            label: label.to_string(),
            act_id,
            status: status.to_string(),
        });
    }
    driven
}

/// The element naming `act_id` in one page.
fn element(page: &[Value], act_id: &str, label: &str) -> Value {
    page.iter()
        .find(|summary| summary["actId"] == act_id)
        .unwrap_or_else(|| panic!("case {label:?}: act {act_id} must be listed in {page:?}"))
        .clone()
}

/// RULE: one page reports, for each act it names, the status that act was driven into — so
/// the page carries every status its acts hold rather than one value for all of them.
#[tokio::test]
async fn one_page_reports_each_act_in_the_status_it_was_driven_into() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;
    let driven = driven_acts(&emulator, &chain).await;

    let (code, body) = list(&emulator, DEFINITION, &Query::default()).await;
    assert_eq!(code, 200, "expected HTTP 200, got {code} {body}");
    let page = summaries(&body);
    assert_eq!(
        page.len(),
        driven.len(),
        "expected one summary per stored act, got {body}"
    );

    for act in &driven {
        let summary = element(&page, &act.act_id, &act.label);
        assert_eq!(
            summary["status"], act.status,
            "case {:?}: expected the status the act was driven into ({}), got {summary}",
            act.label, act.status
        );
        assert_eq!(
            summary["endedAt"].is_null(),
            !finishes(&act.status),
            "case {:?}: endedAt is reported exactly for an act that has finished ({}), got \
             {summary}",
            act.label,
            act.status
        );
        // The required members of the element still describe the act's place in the chain.
        assert_eq!(
            summary["workflowRunId"], chain.run_id,
            "case {:?}: expected the run holding the act, got {summary}",
            act.label
        );
        assert_eq!(
            summary["sessionId"], chain.session_id,
            "case {:?}: expected the session holding the act, got {summary}",
            act.label
        );
        assert!(
            !text(&summary, "startedAt").is_empty(),
            "case {:?}: startedAt is required of every summary, got {summary}",
            act.label
        );
    }

    // Every value the model's enum declares is held by one of the listed acts, so a page
    // that carried a single status for all of them would fail on all but one element.
    let reported: BTreeSet<String> = page
        .iter()
        .map(|summary| text(summary, "status"))
        .collect();
    let declared: BTreeSet<String> = ACT_STATUS_VALUES.iter().map(|v| v.to_string()).collect();
    assert_eq!(
        reported, declared,
        "expected the page to report every status ActStatus declares, got {reported:?}"
    );

    // The listing reports state: reading it again reports exactly the same page.
    let (code, again) = list(&emulator, DEFINITION, &Query::default()).await;
    assert_eq!(code, 200, "expected HTTP 200, got {code} {again}");
    assert_eq!(
        summaries(&again),
        page,
        "a second listing must report the same acts in the same statuses, got {again}"
    );
}

/// RULE: the status is each element's own, so moving ONE act moves only the element that
/// names it.
#[tokio::test]
async fn moving_one_act_moves_only_the_element_naming_it() {
    let emulator = Emulator::new("eu-west-1");
    let chain = chain(&emulator, DEFINITION).await;
    let driven = driven_acts(&emulator, &chain).await;

    let (code, body) = list(&emulator, DEFINITION, &Query::default()).await;
    assert_eq!(code, 200, "expected HTTP 200, got {code} {body}");
    let before = summaries(&body);

    // Each act still under way is moved in turn, and after each move every OTHER element is
    // still exactly what it was.
    let mut expected: BTreeMap<String, String> = driven
        .iter()
        .map(|act| (act.act_id.clone(), act.status.clone()))
        .collect();
    for act in driven.iter().filter(|act| !finishes(&act.status)) {
        let requested = "SUCCEEDED";
        let (code, answer) = move_to(&emulator, &chain, &act.act_id, requested).await;
        assert_eq!(
            code, 200,
            "case {:?}: the update must be accepted, got {code} {answer}",
            act.label
        );
        expected.insert(act.act_id.clone(), requested.to_string());

        let (code, body) = list(&emulator, DEFINITION, &Query::default()).await;
        assert_eq!(code, 200, "expected HTTP 200, got {code} {body}");
        let after = summaries(&body);
        for (act_id, status) in &expected {
            let summary = element(&after, act_id, &act.label);
            assert_eq!(
                summary["status"], *status,
                "case {:?}: expected act {act_id} to report {status}, got {summary}",
                act.label
            );
        }
        // The elements of the acts this move did not name are unchanged, instants included.
        for other in driven.iter().filter(|other| other.act_id != act.act_id) {
            let was = element(&before, &other.act_id, &other.label);
            let now = element(&after, &other.act_id, &other.label);
            if expected[&other.act_id] == other.status {
                assert_eq!(
                    now, was,
                    "case {:?}: act {} was not named by the update and must be unchanged, got \
                     {now}",
                    act.label, other.act_id
                );
            }
        }
    }
}

/// RULE: the projection is per element, not per response — each page of a split listing,
/// in either sort order, still carries each act's own status.
#[tokio::test]
async fn every_page_of_either_sort_order_carries_each_acts_own_status() {
    let emulator = Emulator::new("ap-southeast-1");
    let chain = chain(&emulator, DEFINITION).await;
    let driven = driven_acts(&emulator, &chain).await;
    let expected: BTreeMap<String, String> = driven
        .iter()
        .map(|act| (act.act_id.clone(), act.status.clone()))
        .collect();

    // `SortOrder` is one of Ascending | Descending
    // (`smithy_query(query="shape",shape="SortOrder")`); no member at all is the third row.
    for order in [None, Some("Ascending"), Some("Descending")] {
        let label = order.unwrap_or("no sortOrder");
        let mut collected: BTreeMap<String, String> = BTreeMap::new();
        let mut token: Option<String> = None;
        // One page per act, plus the page that reports the listing is exhausted.
        for page_index in 0..=driven.len() {
            let query = Query {
                session_id: Some(chain.session_id.clone()),
                max_results: Some(1),
                next_token: token.clone(),
                sort_order: order.map(str::to_string),
                ..Query::default()
            };
            let (code, body) = list(&emulator, DEFINITION, &query).await;
            assert_eq!(
                code, 200,
                "case {label:?} page {page_index}: expected HTTP 200, got {code} {body}"
            );
            for summary in summaries(&body) {
                let act_id = text(&summary, "actId");
                let status = text(&summary, "status");
                let wanted = expected.get(&act_id).unwrap_or_else(|| {
                    panic!("case {label:?} page {page_index}: unknown act {act_id} in {body}")
                });
                assert_eq!(
                    &status, wanted,
                    "case {label:?} page {page_index}: expected act {act_id} to report \
                     {wanted}, got {summary}"
                );
                assert_eq!(
                    summary["endedAt"].is_null(),
                    !finishes(&status),
                    "case {label:?} page {page_index}: endedAt is reported exactly for an act \
                     that has finished ({status}), got {summary}"
                );
                collected.insert(act_id, status);
            }
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {label:?}: the listing must be exhausted after one page per act, got a \
             further token {token:?}"
        );
        assert_eq!(
            collected, expected,
            "case {label:?}: every act must be reported once, in its own status"
        );
    }
}
