// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PUBLIC form of the values an execution read reports,
//! driven over each service's own wire protocol through the production
//! `MultiServiceRouter`, so what is asserted is what a signed AWS SDK request sees.
//!
//! The rule under test: a read answers every value in the public form its model gives
//! that value, never in the representation this store happens to keep it in. Two
//! surfaces of that one rule:
//!
//! 1. a `nextToken` is a `PageToken` — a cursor the caller hands back unread — so a
//!    paged read of an execution's history answers a token that carries the position
//!    without showing it, continues exactly that history when it comes back, and
//!    refuses a position a caller wrote itself with the modelled `InvalidToken`;
//! 2. a KMS key reference a resource is configured by ("an alias, alias ARN, key ID,
//!    or key ARN", `smithy_query(query="shape",shape="EncryptionConfiguration")`) is
//!    reported as the ARN of the key it names, so the configuration an execution ran
//!    reads back as the key itself whichever of those forms configured it.
//!
//! The rows span what each rule generalises over: the page sizes and both sort orders
//! a history can be paged in for the first, and all four reference forms — over an
//! execution started on the state machine itself and on a published version of it,
//! beside a resource on no customer managed key at all — for the second.
//!
//! The harnesses (`Emulator`, `Chain`, `create_machine`, `start_execution`) are the
//! ones `state_machine_lifecycle` and `execution_data_encryption` build;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_EXECUTION_SUCCEEDED;

use crate::error_messages::failure;
use crate::execution_data_encryption::{customer_managed, granting_policy, Chain};
use crate::state_machine_lifecycle::{
    create_machine, start_execution, string, Emulator, ACCOUNT, HELLO_DEFINITION, REGION, ROLE_ARN,
};

/// A definition whose three `Pass` states leave a history long enough to page several
/// ways: an `ExecutionStarted`, an entered/exited pair for each state, and the
/// outcome.
const THREE_PASS_DEFINITION: &str = r#"{
  "StartAt": "One",
  "States": {
    "One": { "Type": "Pass", "Result": { "step": 1 }, "Next": "Two" },
    "Two": { "Type": "Pass", "Result": { "step": 2 }, "Next": "Three" },
    "Three": { "Type": "Pass", "Result": { "step": 3 }, "End": true }
  }
}"#;

/// The widest a `nextToken` of this operation may be: `PageToken` is `@length 1..1024`
/// (`smithy_query(query="shape",shape="PageToken")`).
const MAX_PAGE_TOKEN_LENGTH: usize = 1024;

/// The modelled refusal a token this service did not issue answers, and its status:
/// "Using an expired pagination token will return an HTTP 400 InvalidToken error".
const INVALID_TOKEN: &str = "InvalidToken";
const INVALID_TOKEN_STATUS: u16 = 400;

/// How long a wait for the clock to make an execution's outcome due may take before
/// the test gives up, and how often it looks again. Generous: the condition is what is
/// waited for, not a span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The `type` of each event, in the order they were answered.
fn types(events: &[Value]) -> Vec<String> {
    events.iter().map(|event| string(event, "type")).collect()
}

/// The `events` member of one answered page.
fn events_of(page: &Value) -> Vec<Value> {
    page.get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an events list in {page:?}"))
        .clone()
}

/// The whole history of `execution`, once the clock has made its outcome due: the read
/// is repeated until the terminal event is there, so nothing here depends on how fast
/// the host runs.
async fn settled_history(emulator: &Emulator, case: &str, execution: &str) -> Vec<Value> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let events = events_of(
            &emulator
                .ok("GetExecutionHistory", &json!({ "executionArn": execution }))
                .await,
        );
        if types(&events)
            .last()
            .is_some_and(|last| last == EVENT_EXECUTION_SUCCEEDED)
        {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution's outcome to be written, got {:?}",
            types(&events)
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One page of this history: `maxResults` events, in the order `reverse_order` asks
/// for, continuing from `token` when there is one.
async fn page(
    emulator: &Emulator,
    execution: &str,
    max_results: i64,
    reverse_order: bool,
    token: Option<&str>,
) -> Value {
    let mut request = json!({
        "executionArn": execution,
        "maxResults": max_results,
        "reverseOrder": reverse_order,
    });
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    emulator.ok("GetExecutionHistory", &request).await
}

/// Rule 1: the `nextToken` a paged history read answers is an opaque `PageToken` — it
/// is not the position it stands for, it is within the modelled token length, and
/// handing it back continues exactly the order the read walks — while a position a
/// caller writes itself is the modelled `InvalidToken`.
///
/// The rows differ along what the rule generalises over: the page size a read is cut
/// into (smaller than, dividing, not dividing and larger than the history) and both
/// sort orders, which is the other thing a history token is bound to.
#[tokio::test]
async fn a_paged_history_answers_an_opaque_token_at_every_page_size() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "opaque-history-tokens", THREE_PASS_DEFINITION).await;
    let execution = start_execution(&emulator, &machine, "paged", "{}").await;
    let whole = types(&settled_history(&emulator, "the walked execution", &execution).await);
    assert!(whole.len() >= 4, "expected a history to page, got {whole:?}");

    for reverse_order in [false, true] {
        let expected: Vec<String> = if reverse_order {
            whole.iter().rev().cloned().collect()
        } else {
            whole.clone()
        };
        for max_results in [1_i64, 2, 3, 4, i64::try_from(whole.len() + 1).unwrap_or(i64::MAX)] {
            let case = format!("maxResults {max_results}, reverseOrder {reverse_order}");
            let mut walked: Vec<String> = Vec::new();
            let mut handed: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let answered = page(
                    &emulator,
                    &execution,
                    max_results,
                    reverse_order,
                    token.as_deref(),
                )
                .await;
                walked.extend(types(&events_of(&answered)));
                let next = answered
                    .get("nextToken")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let Some(next) = next else { break };
                // A token is the caller's handle on the rest of the listing, not the
                // position it stands for: no decimal index, and nothing any position
                // of this history spells.
                assert!(
                    next.trim().parse::<u64>().is_err(),
                    "case {case:?}: expected an opaque token, got the position {next:?}"
                );
                assert!(
                    (1..=MAX_PAGE_TOKEN_LENGTH).contains(&next.len()),
                    "case {case:?}: expected a token of a modelled length, got {} characters \
                     in {next:?}",
                    next.len()
                );
                assert!(
                    !handed.contains(&next),
                    "case {case:?}: expected each page's token to be its own, got {next:?} \
                     again after {handed:?}"
                );
                handed.push(next.clone());
                token = Some(next);
                // A page of this walk is at least one event long and the walk covers the
                // history once, so neither the events seen nor the pages taken can
                // outrun the history itself: a read that answered a token without
                // advancing fails here rather than looping.
                assert!(
                    walked.len() <= expected.len() && handed.len() <= expected.len(),
                    "case {case:?}: expected the pages to walk the history once, got {walked:?} \
                     over {} page(s)",
                    handed.len()
                );
            }
            // The opaque token really carries the cursor: the pages it walks are the
            // history itself, in the order this read asked for.
            assert_eq!(
                walked, expected,
                "case {case:?}: expected the pages to concatenate to the history, got {walked:?}"
            );
        }
    }

    // What a caller might write in place of a token it was handed: every position of
    // this history, and text that is no token at all.
    let mut candidates: Vec<(String, String)> = (0..=whole.len())
        .map(|position| (format!("the position {position}"), position.to_string()))
        .collect();
    candidates.push(("text that is no token".to_string(), "not-a-token".to_string()));
    for (what, candidate) in candidates {
        let (status, code, message) = failure(
            &emulator,
            &what,
            "GetExecutionHistory",
            &json!({
                "executionArn": execution.clone(),
                "maxResults": 1,
                "nextToken": candidate.clone(),
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (INVALID_TOKEN_STATUS, INVALID_TOKEN),
            "case {what:?}: expected {INVALID_TOKEN_STATUS} {INVALID_TOKEN} for {candidate:?}, \
             got {status} {code} with message {message:?}"
        );
    }
}

/// A state machine created with `encryption`, and one execution of it — started on the
/// state machine itself, or on the version `published` asks for, which is the OTHER
/// configuration record a start can pin an execution to.
async fn ran(
    chain: &Chain,
    name: &str,
    encryption: Option<Value>,
    published: bool,
) -> (String, String) {
    let mut request = json!({
        "name": name,
        "definition": HELLO_DEFINITION,
        "roleArn": ROLE_ARN,
    });
    if let Some(encryption) = encryption {
        request["encryptionConfiguration"] = encryption;
    }
    let machine = string(
        &chain.sfn("CreateStateMachine", &request).await,
        "stateMachineArn",
    );
    let started_on = if published {
        string(
            &chain
                .sfn(
                    "PublishStateMachineVersion",
                    &json!({ "stateMachineArn": machine }),
                )
                .await,
            "stateMachineVersionArn",
        )
    } else {
        machine.clone()
    };
    let execution = string(
        &chain
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": started_on, "input": "{}" }),
            )
            .await,
        "executionArn",
    );
    (machine, execution)
}

/// Rule 2: `kmsKeyId` accepts "an alias, alias ARN, key ID, or key ARN of a symmetric
/// encryption KMS key" — four ways of naming ONE key — so the configuration an
/// execution RAN reads back with that key's own ARN, whichever form configured it, and
/// a configuration that names no customer managed key reports no key reference at all.
///
/// The rows differ along the reference form the create carried and along which
/// configuration record the start pinned the execution to (the state machine's own, or
/// a published version's).
#[tokio::test]
async fn the_execution_read_reports_the_configured_key_as_its_arn() {
    let chain = Chain::new();
    let key_arn = chain.create_key(&granting_policy()).await;
    let key_id = key_arn
        .split_once("key/")
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| panic!("case the key: expected a key ARN, got {key_arn:?}"));
    let alias = "alias/execution-read-key";
    chain
        .kms(
            "CreateAlias",
            &json!({ "AliasName": alias, "TargetKeyId": key_id }),
        )
        .await;
    // An alias ARN names the alias in the key's own Region and account, which is the
    // form `kmsKeyId` documents for a key in another account.
    let alias_arn = format!("arn:aws:kms:{REGION}:{ACCOUNT}:{alias}");

    // (case, the reference the create carried, whether the start pinned a version)
    let rows: Vec<(&str, String, bool)> = vec![
        ("the key ARN", key_arn.clone(), false),
        ("the bare key id", key_id.clone(), false),
        ("an alias name", alias.to_string(), false),
        ("an alias ARN", alias_arn, false),
        ("the bare key id, of a version", key_id.clone(), true),
        ("an alias name, of a version", alias.to_string(), true),
    ];

    for (index, (case, reference, published)) in rows.into_iter().enumerate() {
        let (machine, execution) = ran(
            &chain,
            &format!("execution-read-key-{index}"),
            Some(customer_managed(&reference)),
            published,
        )
        .await;
        let described = chain
            .sfn(
                "DescribeStateMachineForExecution",
                &json!({ "executionArn": execution }),
            )
            .await;
        let reported = string(&described["encryptionConfiguration"], "kmsKeyId");
        assert_eq!(
            reported, key_arn,
            "case {case:?}: expected the execution read to report the key ARN {key_arn:?}, got \
             {reported:?} in {described:?}"
        );
        assert_ne!(
            reported, key_id,
            "case {case:?}: expected a key ARN rather than the bare key id, got {reported:?}"
        );
        // Both readers answer from the same configuration record, so neither may
        // report the key in a form the other does not.
        let machine_read = chain
            .sfn(
                "DescribeStateMachine",
                &json!({ "stateMachineArn": machine }),
            )
            .await;
        assert_eq!(
            string(&machine_read["encryptionConfiguration"], "kmsKeyId"),
            reported,
            "case {case:?}: expected both reads to report one key reference, got \
             {machine_read:?} against {described:?}"
        );
    }

    // A state machine on no customer managed key names no key, so there is no ARN to
    // report: the read answers the type it is on and no key reference.
    let (_, execution) = ran(&chain, "execution-read-owned-key", None, false).await;
    let described = chain
        .sfn(
            "DescribeStateMachineForExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
    let configuration = &described["encryptionConfiguration"];
    // The `EncryptionType` value is spelled as the model declares it rather than read
    // from the crate, so a wrong constant in the implementation cannot pass here.
    assert_eq!(
        string(configuration, "type"),
        "AWS_OWNED_KEY",
        "case an Amazon Web Services owned key: expected that type, got {described:?}"
    );
    assert!(
        configuration.get("kmsKeyId").is_none_or(Value::is_null),
        "case an Amazon Web Services owned key: expected no key reference, got {described:?}"
    );
}
