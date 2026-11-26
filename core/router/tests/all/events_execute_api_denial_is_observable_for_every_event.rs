// SPDX-License-Identifier: CC0-1.0

//! What a SEQUENCE of matched events comes to when it fans out over two targets of the same
//! deployed API Gateway REST API method whose roles are scoped differently — the span a client
//! that scopes a target's role observes: the method answers the client's own request before any
//! event is published, the permitted twin's delivery reaches what the method integrates with for
//! EVERY event, and the twin whose role may not invoke leaves one dead letter per event.
//!
//! RULE: every matched event is one delivery attempt per target, decided by the documents in force
//! on THAT target's role at THAT attempt and reported on its own — EventBridge invokes an API
//! Gateway target as the role the target names ("for EC2 instances, Kinesis data streams, Step
//! Functions state machines and API Gateway APIs, EventBridge relies on IAM roles that you specify
//! in the `RoleARN` argument in `PutTargets`") — so a role allowed `execute-api:Invoke` on the
//! target's own ARN has every event's request served by the stage, while a role that is not is
//! answered `403 Forbidden` for every event and, under a `RetryPolicy` allowing no retry, writes
//! THAT event once to the target's `DeadLetterConfig` queue with `ERROR_CODE` `NO_PERMISSIONS` and
//! the sentence EventBridge words a status an invoke answered with. No earlier outcome, served or
//! refused, decides a later one: a request at a deployed `AWS_IAM`-authorized method always
//! terminates in that method's answer, whether a client made it or EventBridge did, so each event
//! of the sequence is observable as its own delivery or its own refusal.
//!
//! Only a cross-service suite can pin this: the bus, its rules, their targets and the dead letters
//! are EventBridge's, the stage and its integration are API Gateway's, the user and the roles whose
//! documents decide each request are IAM's, and the sink the method writes into — which is what
//! shows whether anything reached the stage at all — is Amazon SQS's. The stage, its queues, the
//! publication, the scoping that withholds the invoke and the assertion of what a refused delivery
//! leaves are the ones [`crate::events_execute_api_target_role_grant`] already stands up and
//! spells, and the client's own signed dial, the wiring of one target of that method and the
//! reading of a sink message are the ones
//! [`crate::events_execute_api_stage_answers_every_caller`] already makes; both are imported
//! rather than written again. What is this suite's own is the SEQUENCE below.
//!
//! The rows vary what the rule generalises over: the POSITION of an event in the sequence (so a
//! row reads a wiring that has already served one delivery and already refused another), and the
//! FORM of the resource each event addresses — the value its `PathParameterValues` entry takes out
//! of the event and substitutes into the request line. The last leg then varies WHEN the grant is
//! written, after the refused twin has already failed every event before it.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::events_execute_api_stage_answers_every_caller::{
    body_of, served_within, signing_user, wired, Address,
};
use crate::events_execute_api_target_role_grant::{
    assert_distinct_request_ids, assert_refused_dead_letter, deployed, drained, published, Deployed,
    INVOKE_ACTION, OTHER_ACTION,
};
use crate::events_rest_api_stage_target::{
    emulator, granted, messages, policy, role, EVENTS_TRUST,
};

/// The one source every event of the sequence carries, which both twins' rules match: one
/// publication is therefore one attempt at each twin.
const SOURCE: &str = "sim2real.orders.sequence";

/// What the permitted twin contributes as the target's own header parameter, which is how the sink
/// says WHICH twin reached the method.
const PERMITTED_HEADER: &str = "rule-good";

/// What the refused twin contributes, which must never appear at the sink: its request is never
/// served, so nothing it contributes gets behind the method.
const REFUSED_HEADER: &str = "rule-denied";

/// The resources the successive events of the sequence address, which is what each event's
/// `PathParameterValues` entry (`$.detail.id`) takes out of it and substitutes into the request
/// line: one plain word, one padded number, one mixed-case identifier of a different length.
const SEQUENCE: &[&str] = &["w-1", "order-00042", "ORD-Zx-7"];

/// One twin as it stands once wired: the name of its role, what it contributes, whether the stage
/// serves its deliveries and the ARN of the rule that invokes it.
#[derive(Clone, Debug)]
struct Twin {
    role_name: String,
    header: &'static str,
    served: bool,
    rule_arn: String,
}

/// Wire both twins of the sequence to the method of `stage`: the permitted one, whose role is
/// allowed the invoke on the target's own ARN, and the refused one, whose role is allowed only
/// another action there.
async fn twins(router: &MultiServiceRouter, case: &str, stage: &Deployed) -> Vec<Twin> {
    let mut wired_twins: Vec<Twin> = Vec::new();
    for (index, (header, served)) in [(PERMITTED_HEADER, true), (REFUSED_HEADER, false)]
        .into_iter()
        .enumerate()
    {
        let role_name = format!("exec-api-sequence-role-{index}");
        let action = match served {
            true => INVOKE_ACTION,
            false => OTHER_ACTION,
        };
        let role_arn = role(
            router,
            &role_name,
            EVENTS_TRUST,
            Some(&policy(action, &stage.target_arn())),
        )
        .await;
        let rule_arn = wired(
            router,
            case,
            stage,
            &format!("exec-api-sequence-{index}"),
            SOURCE,
            &role_arn,
            header,
        )
        .await;
        wired_twins.push(Twin {
            role_name,
            header,
            served,
            rule_arn,
        });
    }
    wired_twins
}

/// Assert that the sink holds exactly the request every served twin made for the event addressing
/// `id`, and nothing a refused twin contributes, then empty it for the next event.
async fn assert_sink_served(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    twins: &[Twin],
    id: &str,
) {
    let served: Vec<&Twin> = twins.iter().filter(|twin| twin.served).collect();
    let held = messages(router, case, &stage.sink_url, served.len()).await;
    assert_eq!(
        held.len(),
        served.len(),
        "case {case:?}: expected the {} request(s) the permitted target(s) made at the method, \
         got {held:?}",
        served.len()
    );
    for twin in &served {
        let found = held
            .iter()
            .find(|message| body_of(case, message)["orderSrc"].as_str() == Some(twin.header));
        let message = found.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the method to have served the twin contributing {:?}, \
                 got {held:?}",
                twin.header
            )
        });
        let body = body_of(case, message);
        assert_eq!(
            body["pathId"].as_str(),
            Some(id),
            "case {case:?}: expected the twin contributing {:?} to address the resource this \
             event names, got {body}",
            twin.header
        );
    }
    drained(router, case, &stage.sink_url).await;
}

#[tokio::test]
async fn every_event_of_a_sequence_is_served_or_dead_lettered_on_its_own() {
    let router = emulator();
    let stage = deployed(&router, "exec-api-sequence").await;

    // Before anything is published: a client's own signed request at the freshly deployed method,
    // as a user allowed the invoke on the method's own ARN, is answered by the method — the leg
    // whose absence makes a stage that never answers indistinguishable from one that refused.
    let warm_up = "a client's own request at the freshly deployed method";
    let key = signing_user(
        &router,
        "exec-api-sequence-user",
        Some(&policy(INVOKE_ACTION, &stage.target_arn())),
    )
    .await;
    let warm_up_id = "warm-up-1";
    let warm_up_header = "signed-caller";
    let (status, text) = served_within(
        &router,
        &stage.api,
        Address::InvokeHost,
        &key,
        warm_up_id,
        warm_up_header,
        &json!({ "id": warm_up_id, "src": warm_up_header }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {warm_up:?}: expected the method's own answer, got {status} with {text:?}"
    );
    let held = messages(&router, warm_up, &stage.sink_url, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {warm_up:?}: expected the one request the client made at the method, got {held:?}"
    );
    let body = body_of(warm_up, &held[0]);
    assert_eq!(
        body["pathId"].as_str(),
        Some(warm_up_id),
        "case {warm_up:?}: expected the resource the client addressed, got {body}"
    );
    drained(&router, warm_up, &stage.sink_url).await;

    let twins = twins(&router, "the wiring of both twins", &stage).await;
    let mut request_ids: Vec<String> = Vec::new();
    for (position, id) in SEQUENCE.iter().enumerate() {
        let case = format!("the event at position {position} of the sequence, addressing {id:?}");
        published(&router, &case, &stage, SOURCE, id).await;

        assert_sink_served(&router, &case, &stage, &twins, id).await;

        // What the refused twin left for THIS event: one dead letter naming the permission, the
        // answer the invoke endpoint gives such an identity, its own rule and its own target.
        for twin in twins.iter().filter(|twin| !twin.served) {
            request_ids.push(
                assert_refused_dead_letter(
                    &router,
                    &case,
                    &stage,
                    &stage.dead_letter_url,
                    &twin.rule_arn,
                    id,
                )
                .await,
            );
        }
    }
    assert_distinct_request_ids("the sequence", &request_ids);

    // The documents in force on a target's role are read at every delivery, so the grant written
    // now — after that twin has been refused every event of the sequence — decides the next one,
    // with the rules, the targets and the stage untouched.
    let repaired = "the grant written after the twin has been refused every event so far";
    for twin in twins.iter().filter(|twin| !twin.served) {
        granted(
            &router,
            &twin.role_name,
            &policy(INVOKE_ACTION, &stage.target_arn()),
        )
        .await;
    }
    let both_served: Vec<Twin> = twins
        .iter()
        .map(|twin| Twin {
            served: true,
            ..twin.clone()
        })
        .collect();
    // TWO more events over the repaired wiring, because the dead-letter queue is read here for
    // what it does NOT hold, and it was left empty by the sequence above: a delivery is made
    // inside the `PutEvents` that triggered it, and once the deliveries of the LATER event have
    // been served, every delivery of the earlier one has already come to whatever it comes to —
    // so the one read below is the answer for both.
    for id in ["ord-repaired", "ord-repaired-again"] {
        published(&router, repaired, &stage, SOURCE, id).await;
        assert_sink_served(&router, repaired, &stage, &both_served, id).await;
    }
    let dead = messages(&router, repaired, &stage.dead_letter_url, 0).await;
    assert!(
        dead.is_empty(),
        "case {repaired:?}: a delivery the method served dead-letters nothing, got {dead:?}"
    );
}
