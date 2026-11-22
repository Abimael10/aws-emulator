// SPDX-License-Identifier: CC0-1.0

//! The BODY of a `nextToken` refusal, over EVERY paginated operation of this
//! service.
//!
//! The modelled `NextToken` pattern (`^[A-Za-z0-9+/=]+$`) admits any string of
//! those characters, so a token this service never minted reaches the handler,
//! which refuses it itself. The rule under test is that such a refusal is the
//! crate's single validation report for one member — "`<n>` validation error(s)
//! detected: Value at '`<member>`' failed to satisfy constraint: `<requirement>`"
//! — with the rejected token echoed nowhere in it, whichever paginated
//! operation was called and whichever way the token fails.
//!
//! The cases span the two ways a token can fail: one this service could not have
//! minted at all (it does not decode), and one that decodes but was taken for a
//! different query than the call resuming with it. The first is driven over a
//! TABLE of token forms, the second both synthetically (a cursor carrying a
//! fingerprint no query of this service produces) and end-to-end from a token a
//! previous page really issued.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizerautomation_computeoptimizerautomationservice::ops::types::RecommendedAction;
use service_com_amazonaws_computeoptimizerautomation_computeoptimizerautomationservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::automation_rule_lifecycle::{
    call, call_ok, emulator_with_engine, error_code, member, minimal_rule, string_member,
};
use super::error_payloads::error_message;

/// The requirement clause of a refusal of a token this service never issued, for
/// the operation that was called.
fn unissued_requirement(operation: &str) -> String {
    format!("Member must be a token returned by a previous {operation} call")
}

/// The whole message a `nextToken` refusal must carry: the crate's validation
/// report for one member, which names the member and the requirement and echoes
/// no value.
fn expected_message(requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at 'nextToken' failed to satisfy constraint: \
         {requirement}"
    )
}

/// Call `operation` with `token` spliced into `body` as its `nextToken`, assert
/// the refusal — HTTP 400, `InvalidParameterValueException`, and exactly
/// `requirement` reported on `nextToken` — and answer the message it carried, so
/// a caller can assert about the body as well.
pub(crate) async fn assert_token_refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    token: &str,
    requirement: &str,
    case: &str,
) -> String {
    let mut request = body.clone();
    request["nextToken"] = json!(token);
    let (status, parsed) = call(router, operation, &request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} and {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "InvalidParameterValueException",
        "case {case:?}: expected error code InvalidParameterValueException, got {parsed}"
    );
    let message = error_message(&parsed);
    assert_eq!(
        message,
        expected_message(requirement),
        "case {case:?}: expected message {:?}, got {message:?}",
        expected_message(requirement)
    );
    message
}

/// One recommended action Compute Optimizer's analysis would have produced, of
/// the type whose automation performs more than one step, so a page of its steps
/// can leave another behind it.
fn recommended_action(id: &str, volume: &str) -> RecommendedAction {
    RecommendedAction {
        recommended_action_id: Some(id.to_string()),
        resource_id: Some(volume.to_string()),
        account_id: Some(engine::EMULATOR_ACCOUNT_ID.to_string()),
        region: Some("us-east-1".to_string()),
        // The model's only `ResourceType` value, and the `RecommendedActionType`
        // whose event performs two steps
        // (`smithy_query(query="shape",shape="ResourceType,RecommendedActionType")`).
        resource_type: Some("EbsVolume".to_string()),
        recommended_action_type: Some("SnapshotAndDeleteUnattachedEbsVolume".to_string()),
        ..Default::default()
    }
}

/// Seed `action` where Compute Optimizer's analysis would have left it, then
/// start its automation over the wire and answer the event's id.
async fn start_event(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    action_id: &str,
    volume: &str,
    case: &str,
) -> String {
    let action = recommended_action(action_id, volume);
    let service_state = state::service_state(&**engine).expect("the service state is reachable");
    service_state
        .lock()
        .recommended_actions
        .insert(action_id.to_string(), action);
    let started = call_ok(
        router,
        "StartAutomationEvent",
        &json!({"recommendedActionId": action_id}),
        case,
    )
    .await;
    string_member(&started, "eventId")
}

/// Every paginated operation of this service, as `(operation, a request with
/// every required member and nothing else, the requirement reported for a token
/// taken for a DIFFERENT query)`.
///
/// The operations are the ones the model marks `@paginated`
/// (`smithy_query(query="digest")`); `ListAccounts` takes no query at all, so a
/// token not taken for its own walk is the same refusal as one it never issued.
pub(crate) fn paginated_operations(event_id: &str) -> Vec<(&'static str, Value, &'static str)> {
    let preview_selection = json!({
        "ruleType": "AccountRule",
        "recommendedActionTypes": ["UpgradeEbsVolumeType"],
    });
    vec![
        (
            "ListAccounts",
            json!({}),
            "Member must be a token returned by a previous ListAccounts call",
        ),
        (
            "ListAutomationEvents",
            json!({}),
            "Member must be a token returned for the same filters and time range",
        ),
        (
            "ListAutomationEventSteps",
            json!({"eventId": event_id}),
            "Member must be a token returned for the same eventId",
        ),
        (
            "ListAutomationEventSummaries",
            json!({}),
            "Member must be a token returned for the same filters and dates",
        ),
        (
            "ListAutomationRules",
            json!({}),
            "Member must be a token returned for the same filters",
        ),
        (
            "ListAutomationRulePreview",
            preview_selection.clone(),
            "Member must be a token returned for the same rule type, action types and criteria",
        ),
        (
            "ListAutomationRulePreviewSummaries",
            preview_selection,
            "Member must be a token returned for the same rule type, action types and criteria",
        ),
        (
            "ListRecommendedActions",
            json!({}),
            "Member must be a token returned for the same filters",
        ),
        (
            "ListRecommendedActionSummaries",
            json!({}),
            "Member must be a token returned for the same filters",
        ),
    ]
}

/// Token forms this service cannot have minted, as `(case, token, distinctive)`.
///
/// Every one satisfies the modelled `NextToken` pattern, so it passes the
/// protocol boundary and reaches the handler, and none of them decodes to a
/// cursor. `distinctive` says the token is a string the refusal could only
/// contain by echoing it — a single hex digit can occur in the wording by
/// chance, so an echo check on it would pass for the wrong reason.
const UNISSUED_TOKENS: &[(&str, &str, bool)] = &[
    ("plain-words", "notatokenweissued", true),
    ("one-character", "A", false),
    ("odd-length", "abc", true),
    // Hex of bytes that are not text at all.
    ("not-text", "deadbeef", true),
    // Hex of "0000": text, but no cursor separator.
    ("no-separator", "30303030", true),
    // Hex of "abc:def": a separator, but no offset in front of it.
    ("no-offset", "6162633a646566", true),
    ("padding-only", "==", true),
    (
        "far-longer-than-any-token-we-mint",
        "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        true,
    ),
];

/// A `nextToken` this service never issued is refused, on every paginated
/// operation, with the crate's validation report for `nextToken` — and the
/// rejected token appears nowhere in the body.
#[tokio::test]
async fn an_unissued_page_token_is_refused_without_being_echoed() {
    let (router, engine) = emulator_with_engine();
    let event_id = start_event(&router, &engine, "ActioN0000000001", "vol-0a1b2c3d4", "seed").await;

    for (operation, body, _) in paginated_operations(&event_id) {
        for (token_case, token, distinctive) in UNISSUED_TOKENS {
            let case = format!("{operation}-{token_case}");
            let message = assert_token_refused(
                &router,
                operation,
                &body,
                token,
                &unissued_requirement(operation),
                &case,
            )
            .await;
            if *distinctive {
                assert!(
                    !message.contains(token),
                    "case {case:?}: the refusal must not echo the rejected token, got {message:?}"
                );
            }
        }
    }
}

/// A token that DECODES but was taken for another query is refused with the
/// requirement that names what it has to match, on every paginated operation,
/// again without echoing it.
///
/// The cursor is encoded here with a fingerprint no query of this service
/// produces, which is what every operation's token check compares against.
#[tokio::test]
async fn a_page_token_taken_for_another_query_is_refused() {
    let (router, engine) = emulator_with_engine();
    let event_id = start_event(&router, &engine, "ActioN0000000002", "vol-0a1b2c3d5", "seed").await;
    let foreign = state::encode_page_token(&state::PageCursor {
        offset: 0,
        filter_fingerprint: "nofingerprintofanyqueryofthisservice".to_string(),
    });

    for (operation, body, requirement) in paginated_operations(&event_id) {
        let case = format!("{operation}-foreign-fingerprint");
        let message =
            assert_token_refused(&router, operation, &body, &foreign, requirement, &case).await;
        assert!(
            !message.contains(&foreign),
            "case {case:?}: the refusal must not echo the rejected token, got {message:?}"
        );
    }
}

/// A token a previous page really issued carries the query it was taken with:
/// presented to a different query it is refused with the same report, and
/// presented to the query that issued it, it resumes the walk.
///
/// One row per kind of query a page is a window onto — the steps of ONE event,
/// a filtered event listing, and a filtered rule listing — so the rule is
/// asserted on tokens this service minted, not only on ones the test built.
#[tokio::test]
async fn a_minted_token_resumes_its_own_query_and_is_refused_by_any_other() {
    let (router, engine) = emulator_with_engine();
    let first_event =
        start_event(&router, &engine, "ActioN0000000003", "vol-0a1b2c3d6", "first").await;
    let second_event =
        start_event(&router, &engine, "ActioN0000000004", "vol-0a1b2c3d7", "second").await;
    for name in ["token-rule-one", "token-rule-two"] {
        call_ok(&router, "CreateAutomationRule", &minimal_rule(name), name).await;
    }

    // (case, operation, the query the token is taken with, the query it is then
    // presented to, the requirement that refusal reports)
    let cases: Vec<(&str, &str, Value, Value, &str)> = vec![
        (
            "steps-of-another-event",
            "ListAutomationEventSteps",
            json!({"eventId": first_event.clone(), "maxResults": 1}),
            json!({"eventId": second_event.clone(), "maxResults": 1}),
            "Member must be a token returned for the same eventId",
        ),
        (
            "events-under-a-filter-the-token-was-not-taken-with",
            "ListAutomationEvents",
            json!({"maxResults": 1}),
            json!({"maxResults": 1, "filters": [{"name": "EventType", "values": ["UpgradeEbsVolumeType"]}]}),
            "Member must be a token returned for the same filters and time range",
        ),
        (
            "rules-under-a-filter-the-token-was-not-taken-with",
            "ListAutomationRules",
            json!({"maxResults": 1}),
            json!({"maxResults": 1, "filters": [{"name": "Name", "values": ["token-rule-two"]}]}),
            "Member must be a token returned for the same filters",
        ),
    ];

    for (case, operation, issuing_query, other_query, requirement) in cases {
        let first_page = call_ok(&router, operation, &issuing_query, case).await;
        let token = member(&first_page, "nextToken")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the first page must carry a token, got {first_page}")
            })
            .to_string();

        let message =
            assert_token_refused(&router, operation, &other_query, &token, requirement, case).await;
        assert!(
            !message.contains(&token),
            "case {case:?}: the refusal must not echo the rejected token, got {message:?}"
        );

        // The same token on the query that issued it is served, so the refusal
        // above is the fingerprint check and not a blanket rejection.
        let mut resumed = issuing_query.clone();
        resumed["nextToken"] = json!(token);
        let (status, parsed) = call(&router, operation, &resumed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the issuing query must resume with its own token, got {status} and \
             {parsed}"
        );
    }
}

/// The offsets a cursor can name, spanning the start of a walk, the first
/// resumption, a position well into a sequence and one far past any page this
/// service's fixtures mint.
const CURSOR_OFFSETS: [usize; 4] = [0, 1, 7, 10_000];

/// Token forms that are no cursor of this service: each satisfies the modelled
/// `NextToken` pattern and reaches the handler, and none of them is the one
/// spelling `state::encode_page_token` writes — text that is not hex, a single
/// hex digit, an odd number of them, hex of bytes that are not text, hex of text
/// without the cursor separator, and padding alone.
const UNDECODABLE_TOKENS: [&str; 6] = [
    "notatokenweissued",
    "A",
    "abc",
    "deadbeef",
    "30303030",
    "==",
];

/// The member each paginated operation reports its page in, as that operation's
/// `@paginated(items: …)` names it
/// (`smithy_query(query="operation",operation="ListAccounts,ListAutomationEvents,ListAutomationEventSteps,ListAutomationEventSummaries,ListAutomationRules,ListAutomationRulePreview,ListAutomationRulePreviewSummaries,ListRecommendedActions,ListRecommendedActionSummaries")`).
const PAGE_ITEMS: [(&str, &str); 9] = [
    ("ListAccounts", "accounts"),
    ("ListAutomationEvents", "automationEvents"),
    ("ListAutomationEventSteps", "automationEventSteps"),
    ("ListAutomationEventSummaries", "automationEventSummaries"),
    ("ListAutomationRules", "automationRules"),
    ("ListAutomationRulePreview", "previewResults"),
    ("ListAutomationRulePreviewSummaries", "previewResultSummaries"),
    ("ListRecommendedActions", "recommendedActions"),
    ("ListRecommendedActionSummaries", "recommendedActionSummaries"),
];

/// The listings that report ONE aggregate row per selection, so their walk ends
/// on its first page at any page size and mints no token: the offset arm of the
/// rule below is driven by every OTHER paginated operation.
const AGGREGATE_LISTINGS: [&str; 2] = [
    "ListRecommendedActionSummaries",
    "ListAutomationRulePreviewSummaries",
];

/// The items `body` reports under `member_name`.
fn page_items(body: &Value, member_name: &str) -> Vec<Value> {
    body.get(member_name)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The member the page of `operation` lands in, from the table above.
fn page_items_member(operation: &str) -> &'static str {
    PAGE_ITEMS
        .iter()
        .find(|(name, _)| *name == operation)
        .map(|(_, member_name)| *member_name)
        .unwrap_or_else(|| panic!("{operation:?} must have a paginated items member"))
}

/// The decision a paginated operation makes on the way INTO its walk, as the one
/// rule every one of them enters through answers it: no token starts at the
/// beginning, a token that is no cursor of this service is the token it never
/// issued, a cursor taken for another walk cannot resume this one, and any other
/// cursor names the offset the previous page stopped at.
///
/// Driven on the rule itself over a table of token forms and offsets, so the
/// three outcomes are pinned where they are decided rather than only through the
/// nine handlers that map them to their own wording.
#[test]
fn the_shared_page_entry_rule_answers_one_outcome_per_token_form() {
    // Two fingerprints this service really mints, differing only in the
    // operation that issued them, so "another query" below is a walk of this
    // service and not an invented string.
    let mine = state::page_fingerprint("ListAccounts", &());
    let another = state::page_fingerprint("ListAutomationRules", &());

    assert_eq!(
        state::page_entry_offset(None, &mine),
        Ok(0),
        "case \"no token\": a request carrying no nextToken must start at the beginning"
    );

    for offset in CURSOR_OFFSETS {
        let token = state::encode_page_token(&state::PageCursor {
            offset,
            filter_fingerprint: mine.clone(),
        });
        let entered = state::page_entry_offset(Some(&token), &mine);
        assert_eq!(
            entered,
            Ok(offset),
            "case \"own cursor at {offset}\": expected the cursor's offset {offset}, got \
             {entered:?}"
        );

        let foreign = state::encode_page_token(&state::PageCursor {
            offset,
            filter_fingerprint: another.clone(),
        });
        let entered = state::page_entry_offset(Some(&foreign), &mine);
        assert_eq!(
            entered,
            Err(state::PageEntryRefusal::OtherQuery),
            "case \"cursor of another query at {offset}\": expected the other-query refusal, got \
             {entered:?}"
        );
    }

    for token in UNDECODABLE_TOKENS {
        let entered = state::page_entry_offset(Some(token), &mine);
        assert_eq!(
            entered,
            Err(state::PageEntryRefusal::Unissued),
            "case {token:?}: expected the unissued-token refusal, got {entered:?}"
        );
    }
}

/// Every paginated operation of this service enters its walk through that one
/// rule, each supplying only its own wording: over the wire, on all nine, a
/// request with no token is served from the beginning, a token that does not
/// decode is refused as one this operation never issued, a cursor taken for
/// another query is refused with this operation's own requirement, and the
/// cursor of a token a page really minted resumes after the items that page
/// reported.
#[tokio::test]
async fn every_paginated_operation_enters_its_walk_through_the_shared_rule() {
    let (router, event_id) = crate::shared_page_walk::emulator_with_a_fixture().await;
    // A cursor that decodes but names a walk of no operation of this service, so
    // every listing compares it against the fingerprint it would itself mint.
    let foreign_query = state::encode_page_token(&state::PageCursor {
        offset: 0,
        filter_fingerprint: "nofingerprintofanyqueryofthisservice".to_string(),
    });

    for (operation, request, requirement) in paginated_operations(&event_id) {
        let items_member = page_items_member(operation);

        // No token: the walk starts at the beginning, and the fixture leaves
        // every listing something to report there.
        let case = format!("{operation}-no-token");
        let whole = call_ok(&router, operation, &request, &case).await;
        let served = page_items(&whole, items_member);
        assert!(
            !served.is_empty(),
            "case {case:?}: a request with no nextToken must be served the first page of the \
             walk, got {whole}"
        );

        // A token that is no cursor of this service: the refusal this operation
        // words for a token it never issued.
        for token in UNDECODABLE_TOKENS {
            let case = format!("{operation}-undecodable-{token}");
            assert_token_refused(
                &router,
                operation,
                &request,
                token,
                &unissued_requirement(operation),
                &case,
            )
            .await;
        }

        // A cursor taken for another walk: this operation's own requirement.
        let case = format!("{operation}-cursor-of-another-query");
        assert_token_refused(
            &router,
            operation,
            &request,
            &foreign_query,
            requirement,
            &case,
        )
        .await;

        // The offset arm: a page of one item mints a cursor, and that cursor is
        // where the walk resumes, so the resumed page reports items the first
        // page did not.
        let case = format!("{operation}-resumes-at-its-cursor");
        let mut smallest_page = request.clone();
        smallest_page["maxResults"] = json!(1);
        let first = call_ok(&router, operation, &smallest_page, &case).await;
        let minted = member(&first, "nextToken").as_str().map(str::to_string);
        assert_eq!(
            minted.is_some(),
            !AGGREGATE_LISTINGS.contains(&operation),
            "case {case:?}: a listing that reports more than one row must mint a token at a page \
             of one and an aggregate listing must mint none, got {first}"
        );
        if let Some(token) = minted {
            let mut resumed_request = smallest_page.clone();
            resumed_request["nextToken"] = json!(token);
            let (status, resumed) = call(&router, operation, &resumed_request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the query that minted the token must resume with it, got {status} \
                 and {resumed}"
            );
            let (before, after) = (
                page_items(&first, items_member),
                page_items(&resumed, items_member),
            );
            assert_ne!(
                after, before,
                "case {case:?}: the cursor's offset is where the walk resumes, so the resumed \
                 page must report items the first page did not; got {after:?} for both"
            );
        }
    }
}
