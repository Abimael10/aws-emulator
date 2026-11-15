// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the three refusals of its session slice that are CONFLICTS over
//! a claim — a duplicate `invocationId`, a session already terminated, and a
//! `(invocationIdentifier, invocationStepId)` pair already stored — asserted end-to-end
//! over the wire, both when the second caller arrives after the first and when N callers
//! arrive at once.
//!
//! Every case drives a real `restJson1` request through the `MultiServiceRouter` the
//! production emulator serves with (the harness in [`crate::sessions`]), so what is
//! pinned here is the status, the error code and the `message` a Bedrock Agent Runtime
//! client reads — not a handler's return value.
//!
//! THE RULE UNDER TEST: a conflict refusal states the service's own FIXED sentence for
//! that conflict class, whose only variable parts are values the REQUEST carried. Nothing
//! resolved out of the store enters it — not the session a write ran under, not the status
//! a session is in, not a comparison of the stored content with the submitted one — and
//! the sentence takes no trailing period. The wording does not depend on how the conflict
//! was reached, so the loser of a race reads exactly what a second sequential caller
//! reads:
//!
//! 1. `CreateInvocation` under an `invocationId` already held: `Invocation with ID <id>
//!    already exists`;
//! 2. `EndSession` on a session already terminated: `Cannot end a terminated session`, a
//!    CONSTANT with no variable part at all;
//! 3. `PutInvocationStep` under a pair already stored: `Combination of Invocation with ID
//!    <invocationIdentifier> and Invcoation Step with ID <invocationStepId> already
//!    exists` — the same frame, and the same spelling of the second noun, as the
//!    not-found refusal over that pair.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sessions::{call, create_invocation, create_session, date_time, error_code, label, router};

/// The status and error code every row here expects, from the model:
/// `smithy_query(query="operation",operation="CreateInvocation,EndSession,PutInvocationStep")`
/// -> `ConflictException` httpError 409.
const CONFLICT: (u16, &str) = (409, "ConflictException");

/// A step time rows send when the instant is not what they vary, as epoch seconds.
const STEP_TIME: i64 = 1_783_501_811;

/// A second instant, for the rows that contend under one step id by varying the time
/// rather than the payload.
const OTHER_STEP_TIME: i64 = 1_783_509_004;

/// The `n`th client-supplied id in the canonical form every `Uuid`-shaped member of this
/// model requires: lowercase hex in 8-4-4-4-12 groups
/// (`smithy_query(query="shape",shape="Uuid")` -> `@pattern
/// ^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`). Generated rather than
/// listed, so a row may ask for as many distinct ids as it has callers.
fn uuid_n(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

/// RULE 1: the sentence `CreateInvocation` states for an `invocationId` already held,
/// built from the id the caller chose and nothing else.
fn invocation_already_exists(invocation_id: &str) -> String {
    format!("Invocation with ID {invocation_id} already exists")
}

/// RULE 2: the CONSTANT `EndSession` states for a session that is already terminated. It
/// interpolates nothing at all.
const SESSION_ALREADY_TERMINATED: &str = "Cannot end a terminated session";

/// RULE 3: the sentence `PutInvocationStep` states for a pair already stored — the frame
/// the service also uses for the not-found refusal over the same pair (see
/// `crate::invocation_error_messages`), closing in ` already exists`.
fn step_pair_already_exists(invocation_id: &str, step_id: &str) -> String {
    format!(
        "Combination of Invocation with ID {invocation_id} and Invcoation Step with ID \
         {step_id} already exists"
    )
}

/// The `message` member of an error body, or `""` when there is none.
fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// Assert one refusal: the modelled status and code, the exact sentence, and — the rule's
/// two negative halves — that the sentence carries neither the session the call ran under
/// nor a trailing period.
fn assert_conflict(case: &str, status: u16, body: &Value, expected: &str, session_id: &str) {
    assert_eq!(
        (status, error_code(body)),
        CONFLICT,
        "case {case:?}: expected {CONFLICT:?}, got {status} {body:?}"
    );
    let stated = message(body);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected message {expected:?}, got {stated:?}"
    );
    assert!(
        !stated.contains(session_id),
        "case {case:?}: the refusal must not name the session {session_id}, got {stated:?}"
    );
    assert!(
        !stated.ends_with('.'),
        "case {case:?}: the sentence takes no trailing period, got {stated:?}"
    );
}

/// Run `body` as a `PUT`/`PATCH`/`POST` from each of `callers` concurrent tasks against
/// one shared router, and return every `(caller, status, body)` in completion order.
async fn race(
    router: &Arc<MultiServiceRouter>,
    method: &'static str,
    requests: Vec<(usize, String, Value)>,
) -> Vec<(usize, u16, Value)> {
    let mut running = Vec::with_capacity(requests.len());
    for (caller, path, body) in requests {
        let router = Arc::clone(router);
        running.push(tokio::spawn(async move {
            let (status, answered) = call(&router, method, &path, body).await;
            (caller, status, answered)
        }));
    }
    let mut outcomes = Vec::with_capacity(running.len());
    for handle in running {
        outcomes.push(
            handle
                .await
                .unwrap_or_else(|err| panic!("a racing caller panicked: {err}")),
        );
    }
    outcomes
}

/// The callers of `outcomes` whose status is `expected`, and those whose status is not.
fn split(
    outcomes: &[(usize, u16, Value)],
    expected: u16,
) -> (Vec<usize>, Vec<(usize, u16, Value)>) {
    let winners = outcomes
        .iter()
        .filter(|(_, status, _)| *status == expected)
        .map(|(caller, _, _)| *caller)
        .collect();
    let losers = outcomes
        .iter()
        .filter(|(_, status, _)| *status != expected)
        .cloned()
        .collect();
    (winners, losers)
}

/// RULE 1 on the RACE path: when N callers submit one client-supplied `invocationId` at
/// once, exactly one is created and every loser reads the one duplicate-id sentence, built
/// from the id the request carried. Rows differ along how many callers contend, which id
/// they contend for, and whether they address the session by id or by ARN; one caller is
/// the degenerate row where nobody loses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_loser_of_a_race_for_one_invocation_id_states_the_duplicate_id_sentence() {
    // (callers, the contended id, address the session by its ARN rather than its id)
    let rows: Vec<(usize, String, bool)> = vec![
        (1, uuid_n(1), false),
        (2, uuid_n(2), false),
        (5, uuid_n(3), true),
        (10, uuid_n(4), false),
    ];

    for (callers, invocation_id, by_arn) in rows {
        let router = Arc::new(router());
        let (session_id, session_arn) = create_session(&router, Value::Null).await;
        let identifier = if by_arn { &session_arn } else { &session_id };
        let case = format!(
            "{callers} concurrent CreateInvocation for {invocation_id} in a session addressed \
             by {}",
            if by_arn { "its ARN" } else { "its id" }
        );

        let path = format!("/sessions/{}/invocations/", label(identifier));
        let requests: Vec<(usize, String, Value)> = (0..callers)
            .map(|caller| {
                (
                    caller,
                    path.clone(),
                    json!({
                        "invocationId": invocation_id,
                        "description": format!("racer-{caller}"),
                    }),
                )
            })
            .collect();
        let outcomes = race(&router, "PUT", requests).await;

        let (winners, losers) = split(&outcomes, 201);
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: exactly one caller may claim the id, got {outcomes:?}"
        );
        assert_eq!(
            losers.len(),
            callers - 1,
            "case {case:?}: every other caller must be refused, got {outcomes:?}"
        );
        let expected = invocation_already_exists(&invocation_id);
        for (caller, status, body) in &losers {
            assert_conflict(
                &format!("{case}, loser {caller}"),
                *status,
                body,
                &expected,
                &session_id,
            );
        }

        // The winner's create is the only one that reached the store, and it echoed the
        // id the callers contended for.
        let (status, listed) = call(
            &router,
            "POST",
            &format!("/sessions/{session_id}/invocations/"),
            json!({}),
        )
        .await;
        let stored: Vec<&str> = listed["invocationSummaries"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no invocationSummaries in {listed:?}"))
            .iter()
            .map(|summary| summary["invocationId"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            (status, stored.as_slice()),
            (200, [invocation_id.as_str()].as_slice()),
            "case {case:?}: the session must hold the contended id exactly once, got {listed:?}"
        );
    }
}

/// RULE 2 on the RACE path: when N callers end one `ACTIVE` session at once, exactly one
/// performs the transition and every loser reads the one constant sentence — the SAME
/// string for every row, since it has no variable part. Rows differ along how many callers
/// contend, how the session is addressed, and what the session holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_loser_of_a_race_to_end_one_session_states_the_constant_terminated_sentence() {
    // (callers, address by ARN, the session carries metadata and an invocation)
    let rows: Vec<(usize, bool, bool)> = vec![
        (1, false, false),
        (2, false, false),
        (6, true, false),
        (4, false, true),
    ];

    for (callers, by_arn, populated) in rows {
        let router = Arc::new(router());
        let metadata = if populated {
            json!({ "topic": "demo" })
        } else {
            Value::Null
        };
        let (session_id, session_arn) = create_session(&router, metadata).await;
        if populated {
            create_invocation(&router, &session_id, json!({})).await;
        }
        let identifier = if by_arn { &session_arn } else { &session_id };
        let case = format!(
            "{callers} concurrent EndSession on a {} session addressed by {}",
            if populated { "populated" } else { "empty" },
            if by_arn { "its ARN" } else { "its id" }
        );

        let path = format!("/sessions/{}", label(identifier));
        let requests: Vec<(usize, String, Value)> = (0..callers)
            .map(|caller| (caller, path.clone(), Value::Null))
            .collect();
        let outcomes = race(&router, "PATCH", requests).await;

        let (winners, losers) = split(&outcomes, 200);
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: exactly one caller may end the session, got {outcomes:?}"
        );
        for (caller, status, body) in &losers {
            assert_conflict(
                &format!("{case}, loser {caller}"),
                *status,
                body,
                SESSION_ALREADY_TERMINATED,
                &session_id,
            );
            assert!(
                !message(body).contains("ENDED") && !message(body).contains("EXPIRED"),
                "case {case:?}, loser {caller}: the refusal must not state the stored status, \
                 got {:?}",
                message(body)
            );
        }
        // The losers' sentences are ONE string, whatever each caller sent.
        let stated: BTreeSet<&str> = losers
            .iter()
            .map(|(_, _, body)| message(body))
            .collect::<BTreeSet<&str>>();
        assert!(
            stated.len() <= 1,
            "case {case:?}: the losers must read one sentence, got {stated:?}"
        );

        // The winner's transition is what the store holds, and the session is readable.
        let (status, session) = call(
            &router,
            "GET",
            &format!("/sessions/{session_id}/"),
            Value::Null,
        )
        .await;
        assert_eq!(
            (status, session["sessionStatus"].as_str()),
            (200, Some("ENDED")),
            "case {case:?}: the raced session must be ENDED once, got {status} {session:?}"
        );
    }
}

/// RULE 3 on the RACE path: when N callers write one `(invocationIdentifier,
/// invocationStepId)` pair at once with content of their own, exactly one write is stored
/// and every loser reads the one pair sentence, built from the two ids the request carried.
/// Rows differ along how many callers contend, how the session is addressed, and whether
/// the callers differ in their payload or in their step time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_loser_of_a_race_for_one_step_pair_states_the_pair_sentence() {
    // (callers, address by ARN, the callers differ in the step TIME rather than the
    // payload)
    let rows: Vec<(usize, bool, bool)> = vec![
        (1, false, false),
        (2, false, false),
        (6, true, false),
        (3, false, true),
    ];

    for (callers, by_arn, vary_time) in rows {
        let router = Arc::new(router());
        let (session_id, session_arn) = create_session(&router, Value::Null).await;
        let invocation_id = create_invocation(&router, &session_id, json!({})).await;
        let step_id = uuid_n(callers as u64 + 100);
        let identifier = if by_arn { &session_arn } else { &session_id };
        let case = format!(
            "{callers} concurrent PutInvocationStep for one pair, differing in the {}, in a \
             session addressed by {}",
            if vary_time { "step time" } else { "payload" },
            if by_arn { "its ARN" } else { "its id" }
        );

        let path = format!("/sessions/{}/invocationSteps/", label(identifier));
        let requests: Vec<(usize, String, Value)> = (0..callers)
            .map(|caller| {
                let (step_time, text) = if vary_time {
                    // The same content at a different instant is a different step, so it
                    // contends for the pair too.
                    (
                        OTHER_STEP_TIME + caller as i64,
                        "shared".to_string(),
                    )
                } else {
                    (STEP_TIME, format!("racer-{caller}"))
                };
                (
                    caller,
                    path.clone(),
                    json!({
                        "invocationIdentifier": invocation_id,
                        "invocationStepId": step_id,
                        "invocationStepTime": date_time(step_time),
                        "payload": { "contentBlocks": [{ "text": text }] },
                    }),
                )
            })
            .collect();
        let outcomes = race(&router, "PUT", requests).await;

        let (winners, losers) = split(&outcomes, 201);
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: exactly one write may claim the pair, got {outcomes:?}"
        );
        let expected = step_pair_already_exists(&invocation_id, &step_id);
        for (caller, status, body) in &losers {
            assert_conflict(
                &format!("{case}, loser {caller}"),
                *status,
                body,
                &expected,
                &session_id,
            );
            assert!(
                !message(body).contains("content"),
                "case {case:?}, loser {caller}: the refusal is keyed on the ids, so it must \
                 not speak of content, got {:?}",
                message(body)
            );
        }

        // Exactly one step is stored under the pair, and it is readable through it.
        let (status, listed) = call(
            &router,
            "POST",
            &format!("/sessions/{session_id}/invocationSteps/"),
            json!({ "invocationIdentifier": invocation_id }),
        )
        .await;
        assert_eq!(
            (
                status,
                listed["invocationStepSummaries"].as_array().map(Vec::len)
            ),
            (200, Some(1)),
            "case {case:?}: the losers must have stored nothing, got {status} {listed:?}"
        );
        let (status, got) = call(
            &router,
            "POST",
            &format!("/sessions/{session_id}/invocationSteps/{step_id}"),
            json!({ "invocationIdentifier": invocation_id }),
        )
        .await;
        assert_eq!(
            (status, got["invocationStep"]["invocationStepId"].as_str()),
            (200, Some(step_id.as_str())),
            "case {case:?}: the stored step must be readable through the contended pair, got \
             {status} {got:?}"
        );
    }
}

/// The same three sentences on the SEQUENTIAL path: the wording is a property of the
/// conflict class, not of how many callers were in flight. The rows span each refusal, both
/// session identifier forms, and — for the pair refusal — each way a second write can
/// differ from the stored step.
#[tokio::test]
async fn the_second_caller_of_each_conflict_reads_the_same_sentence_as_a_race_loser() {
    let router = router();
    let (session_id, session_arn) = create_session(&router, Value::Null).await;
    let invocation_id = create_invocation(&router, &session_id, json!({})).await;
    let claimed = uuid_n(7);
    let created = create_invocation(
        &router,
        &session_id,
        json!({ "invocationId": claimed }),
    )
    .await;
    assert_eq!(
        created, claimed,
        "CreateInvocation must keep the supplied invocationId, got {created}"
    );
    let step_id = uuid_n(8);
    let (status, written) = call(
        &router,
        "PUT",
        &format!("/sessions/{session_id}/invocationSteps/"),
        json!({
            "invocationIdentifier": invocation_id,
            "invocationStepId": step_id,
            "invocationStepTime": date_time(STEP_TIME),
            "payload": { "contentBlocks": [{ "text": "stored" }] },
        }),
    )
    .await;
    assert_eq!(
        (status, written["invocationStepId"].as_str()),
        (201, Some(step_id.as_str())),
        "PutInvocationStep: expected 201 echoing the supplied id, got {status} {written:?}"
    );

    for (form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
        // (case, method, the path tail, the body, the sentence expected)
        let rows: Vec<(&str, &str, String, Value, String)> = vec![
            (
                "CreateInvocation re-claiming a held invocationId",
                "PUT",
                "/invocations/".to_string(),
                json!({ "invocationId": claimed, "description": "second" }),
                invocation_already_exists(&claimed),
            ),
            (
                "PutInvocationStep re-claiming a stored pair with other content",
                "PUT",
                "/invocationSteps/".to_string(),
                json!({
                    "invocationIdentifier": invocation_id,
                    "invocationStepId": step_id,
                    "invocationStepTime": date_time(STEP_TIME),
                    "payload": { "contentBlocks": [{ "text": "rewritten" }] },
                }),
                step_pair_already_exists(&invocation_id, &step_id),
            ),
            (
                "PutInvocationStep re-claiming a stored pair at another instant",
                "PUT",
                "/invocationSteps/".to_string(),
                json!({
                    "invocationIdentifier": invocation_id,
                    "invocationStepId": step_id,
                    "invocationStepTime": date_time(OTHER_STEP_TIME),
                    "payload": { "contentBlocks": [{ "text": "stored" }] },
                }),
                step_pair_already_exists(&invocation_id, &step_id),
            ),
        ];

        for (case, method, tail, body, expected) in rows {
            let case = format!("{case}, in a session addressed by {form}");
            let path = format!("/sessions/{}{tail}", label(identifier));
            let (status, refused) = call(&router, method, &path, body).await;
            assert_conflict(&case, status, &refused, &expected, &session_id);
        }
    }

    // EndSession last, since it leaves the session terminal: the first call performs the
    // transition and every later one — by either identifier form — states the constant.
    let (status, ended) = call(
        &router,
        "PATCH",
        &format!("/sessions/{session_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        (status, ended["sessionStatus"].as_str()),
        (200, Some("ENDED")),
        "EndSession: expected 200 ENDED, got {status} {ended:?}"
    );
    for (form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
        for attempt in 1..=2 {
            let case = format!("EndSession #{attempt} on a terminated session addressed by {form}");
            let (status, refused) = call(
                &router,
                "PATCH",
                &format!("/sessions/{}", label(identifier)),
                Value::Null,
            )
            .await;
            assert_conflict(
                &case,
                status,
                &refused,
                SESSION_ALREADY_TERMINATED,
                &session_id,
            );
        }
    }
}
