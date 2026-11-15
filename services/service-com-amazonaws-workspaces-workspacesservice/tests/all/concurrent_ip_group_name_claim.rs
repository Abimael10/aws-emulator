// SPDX-License-Identifier: CC0-1.0

//! What every LOSER of a race for one `GroupName` is told, end to end over the
//! awsJson1_1 wire.
//!
//! The rule this suite pins:
//!
//! > a `CreateIpGroup` refused because another stored group already holds the
//! > requested `GroupName` answers HTTP 400 / `ResourceAlreadyExistsException` with
//! > the sentence this service fixes for that error class — the fixed prefix plus
//! > the `GroupName` the request sent, verbatim, and nothing else: no `GroupId`
//! > this emulator minted, no trailing period, and not the error shape's
//! > documentation sentence. The text is the same for every refused caller,
//! > however many raced and whichever one won.
//!
//! [`crate::ip_access_control_groups`] already pins the STRUCTURE of the race
//! (exactly one winner, exactly one persisted survivor) and
//! [`crate::ip_group_and_alias_refusal_messages`] the message of the sequential
//! duplicate-name refusal; the rows below are the same message rule asserted on the
//! concurrent path, where several callers are refused at once.
//!
//! Each row varies what the rule generalises over: how many callers race, whether
//! the name was already claimed before they started (so EVERY racer loses), and the
//! form, size and script of the name. Every case drives a real
//! `MultiServiceRouter` with this service mounted on one shared engine (the harness
//! is [`crate::workspace_lifecycle`]'s), so each assertion is about what a client
//! would see.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_workspaces_workspacesservice::state::{
    message_ip_group_name_taken, MAX_IP_GROUP_TEXT_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::account_links::error_code;
use crate::workspace_lifecycle::{call, emulator_with_engine};

/// The fixed part of the duplicate-group-name refusal, up to and including the final
/// separator; the one variable part that follows it is the requested `GroupName`.
/// Spelled out here rather than taken from the handler's own helper so a drift in
/// that helper is a failure, not a silently agreeing pair.
const NAME_TAKEN_PREFIX: &str =
    "Encountered an exception while creating IP Group - group already exists for given group name - ";

/// The sentence the model documents for `ResourceAlreadyExistsException` itself
/// ("The specified resource already exists."). A shape's documentation is not the
/// wire message of any one refusal, so no refused racer may answer it.
const SHAPE_DOC_SENTENCE: &str = "The specified resource already exists.";

/// The `message` member of an error body, as the JSON protocols carry it.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Every name the table races for, with what makes it a different input.
///
/// `IpGroupName` carries neither a `@length` nor a `@pattern` trait (from
/// `smithy_query(query="shape", shape="IpGroupName", model_path="services/service-com-amazonaws-workspaces-workspacesservice/smithy-model/com-amazonaws-workspaces-workspacesservice.json")`),
/// so each of these is a name a client can send, and the message must carry it
/// through unchanged.
fn raced_names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name", "grp-race-plain".to_string()),
        ("one character", "x".to_string()),
        (
            "spaces and punctuation",
            "office network (eu), floor 3".to_string(),
        ),
        (
            "the separator the message itself uses",
            "group - already - here".to_string(),
        ),
        ("non-ASCII", "bürö-東京".to_string()),
        ("a name shaped like a group id", "wsipg-ffffffff".to_string()),
        (
            "the longest name this emulator stores",
            "n".repeat(MAX_IP_GROUP_TEXT_CHARS),
        ),
    ]
}

/// What one racer answered.
struct Outcome {
    racer: usize,
    status: u16,
    body: Value,
}

/// Fire `racers` concurrent `CreateIpGroup` calls on `group_name` and collect every
/// answer.
///
/// The requests differ in nothing but their description, so nothing but the name can
/// be what a refusal reports.
async fn race_for_name(
    router: &Arc<MultiServiceRouter>,
    group_name: &str,
    racers: usize,
) -> Vec<Outcome> {
    let mut tasks = Vec::new();
    for racer in 0..racers {
        let router = Arc::clone(router);
        let group_name = group_name.to_string();
        tasks.push(tokio::spawn(async move {
            let request = json!({
                "GroupName": group_name,
                "GroupDesc": format!("racer {racer}"),
                "Tags": [{ "Key": "racer", "Value": racer.to_string() }],
            });
            let (status, body) = call(&router, "CreateIpGroup", &request).await;
            Outcome { racer, status, body }
        }));
    }

    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.expect("the racing task completed"));
    }
    outcomes
}

/// Split the answers into the `GroupId`s accepted and the messages refused,
/// asserting each refusal's status and code along the way.
fn partition(case: &str, outcomes: Vec<Outcome>) -> (Vec<String>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut refusals = Vec::new();
    for Outcome { racer, status, body } in outcomes {
        match status {
            200 => {
                let group_id = body["GroupId"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: racer {racer} was accepted without a GroupId, got {body}")
                });
                accepted.push(group_id.to_string());
            }
            400 => {
                assert_eq!(
                    error_code(&body),
                    "ResourceAlreadyExistsException",
                    "case {case:?}: racer {racer} must be refused with \
                     ResourceAlreadyExistsException, got {body}"
                );
                refusals.push(error_message(&body));
            }
            other => panic!(
                "case {case:?}: racer {racer} must answer 200 or 400, got {other} with {body}"
            ),
        }
    }
    (accepted, refusals)
}

/// Assert every refused racer carries exactly the class sentence for `group_name`,
/// and nothing this emulator minted.
fn assert_class_sentence(case: &str, group_name: &str, accepted: &[String], refusals: &[String]) {
    let expected = format!("{NAME_TAKEN_PREFIX}{group_name}");
    for message in refusals {
        assert_eq!(
            *message, expected,
            "case {case:?}: a refused racer carries the fixed prefix plus the requested \
             GroupName, got {message:?}"
        );
        assert_ne!(
            message.as_str(), SHAPE_DOC_SENTENCE,
            "case {case:?}: the message must be the sentence for this error class, not the \
             error shape's documentation, got {message:?}"
        );
        assert!(
            !message.ends_with('.'),
            "case {case:?}: the class sentence ends with the requested name, not a period, \
             got {message:?}"
        );
        for group_id in accepted {
            assert!(
                !message.contains(group_id.as_str()),
                "case {case:?}: the message must not carry the minted GroupId {group_id:?} of \
                 the group that holds the name, got {message:?}"
            );
        }
        assert!(
            !message.contains("wsipg-") || group_name.contains("wsipg-"),
            "case {case:?}: the message must carry no minted group id, got {message:?}"
        );
    }
    // Identical for every loser: one distinct sentence across the whole race.
    let distinct: BTreeSet<&String> = refusals.iter().collect();
    assert!(
        distinct.len() <= 1,
        "case {case:?}: every refused racer must carry the SAME sentence, got {distinct:?}"
    );
}

/// Every group `DescribeIpGroups` reports under `group_name`.
async fn groups_named(router: &MultiServiceRouter, case: &str, group_name: &str) -> Vec<Value> {
    let (status, body) = call(router, "DescribeIpGroups", &json!({ "MaxResults": 25 })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeIpGroups must answer 200, got {status} with {body}"
    );
    body["Result"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a Result list, got {body}"))
        .iter()
        .filter(|group| group["groupName"] == json!(group_name))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_loser_of_a_race_for_one_name_carries_the_class_sentence() {
    // The rows pair each name form with a different racer count, so the message rule
    // is asserted across both the names a client can send and the number of callers
    // refused at once.
    for (index, (case, group_name)) in raced_names().into_iter().enumerate() {
        let racers = [2_usize, 3, 5, 8][index % 4];
        let case = format!("{case} with {racers} racers");

        // A fresh store per row, so each case is judged only on what it created.
        let (router, _engine) = emulator_with_engine();
        let router = Arc::new(router);

        let outcomes = race_for_name(&router, &group_name, racers).await;
        let (accepted, refusals) = partition(&case, outcomes);

        assert_eq!(
            accepted.len(),
            1,
            "case {case:?}: exactly one racer is accepted, got {accepted:?}"
        );
        assert_eq!(
            refusals.len(),
            racers - 1,
            "case {case:?}: every other racer is refused, got {refusals:?}"
        );
        assert_class_sentence(&case, &group_name, &accepted, &refusals);

        // The terminal read: exactly one group persists under the raced name, and it
        // is the one the accepted call named.
        let holding = groups_named(&router, &case, &group_name).await;
        assert_eq!(
            holding.len(),
            1,
            "case {case:?}: exactly one group persists under the raced name, got {holding:?}"
        );
        assert_eq!(
            holding[0]["groupId"],
            json!(accepted[0]),
            "case {case:?}: the persisted group is the accepted one, got {:?}",
            holding[0]
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_claimed_before_the_race_refuses_every_racer_the_same_way() {
    let (router, _engine) = emulator_with_engine();
    let router = Arc::new(router);

    // Varying the racer count alone, over a name claimed by an earlier sequential
    // create: now NO racer wins, and the sentence each is refused with is the same
    // one a single late create gets.
    for racers in [1_usize, 2, 8] {
        let group_name = format!("grp-claimed-{racers}");
        let case = format!("a pre-claimed name with {racers} racers");
        let (status, body) = call(
            &router,
            "CreateIpGroup",
            &json!({ "GroupName": group_name, "GroupDesc": "the holder" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first create must answer 200, got {status} with {body}"
        );
        let holder = body["GroupId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted GroupId, got {body}"))
            .to_string();

        let outcomes = race_for_name(&router, &group_name, racers).await;
        let (accepted, refusals) = partition(&case, outcomes);
        assert!(
            accepted.is_empty(),
            "case {case:?}: no racer may take a name already held, got {accepted:?}"
        );
        assert_eq!(
            refusals.len(),
            racers,
            "case {case:?}: every racer is refused, got {refusals:?}"
        );
        assert_class_sentence(&case, &group_name, std::slice::from_ref(&holder), &refusals);

        // The sentence the handler's own helper builds is the sentence on the wire,
        // so the two cannot drift apart.
        assert_eq!(
            refusals[0],
            message_ip_group_name_taken(&group_name),
            "case {case:?}: the wire message is the one this service fixes for the class, \
             got {:?}",
            refusals[0]
        );

        let holding = groups_named(&router, &case, &group_name).await;
        assert_eq!(
            holding.len(),
            1,
            "case {case:?}: the pre-claimed group is still the only one under the name, got \
             {holding:?}"
        );
        assert_eq!(
            holding[0]["groupId"],
            json!(holder),
            "case {case:?}: the surviving group is the pre-claimed one, got {:?}",
            holding[0]
        );
    }
}
