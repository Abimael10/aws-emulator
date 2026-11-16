// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for REMOVING a saved CloudWatch Logs Insights query definition, and for
//! the refusal a call that addresses one by id alone answers when the account holds none at
//! that id.
//!
//! The rules under test:
//!
//! * `DeleteQueryDefinition` removes the one definition its `queryDefinitionId` names, so
//!   `DescribeQueryDefinitions` no longer reports it and the definitions beside it are
//!   untouched;
//! * an id that resolves to no definition of this account is refused with HTTP 400
//!   `ResourceNotFoundException`, and the message is the sentence the REFUSING CALL words that
//!   condition with: a call that addresses a definition by its id and nothing else states the
//!   verdict on the id alone, while the call whose `queryDefinitionId` is the optional UPDATE
//!   selector also states what each of the two requests it serves does with the member. The
//!   only per-run field of either is the request id the SDK envelope carries.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! a handler does not actually compute cannot pass.
//!
//! The table spans what the rule generalises over rather than one id: an id the store never
//! minted, an id it minted and has removed, and ids at the published bounds of the member
//! (`QueryId`, `@length` 1..256) — for each of the two calls that resolve one.

use serde_json::{json, Value};

use crate::harness::{envelope_request_id, error_code, Logs};

/// The operations under test.
const PUT: &str = "PutQueryDefinition";
const DELETE: &str = "DeleteQueryDefinition";
const DESCRIBE: &str = "DescribeQueryDefinitions";

/// The modelled error code an id naming no definition is refused under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A query string every saved definition holds, so no case is refused over its syntax.
const QUERY_STRING: &str = "fields @timestamp, @message | limit 5";

/// The VERDICT CloudWatch Logs states on an id that resolves to no definition, which is the
/// whole message of a call that addresses a definition by that id and nothing else. Written
/// out here rather than read from the crate, so a sentence the implementation rewords fails
/// this.
const VERDICT: &str = "Query definition id is invalid.";

/// What the call whose `queryDefinitionId` is the optional UPDATE selector adds to that
/// verdict, because leaving the member out of that call means a create instead.
const UPDATE_GUIDANCE: &str = " If you are updating query definition, please provide correct \
     query definition id; If you are creating new query definition, do not use \
     --query-definition-id as a parameter";

/// Save one definition under `name` and answer the id the put minted.
async fn saved(logs: &Logs, name: &str) -> String {
    let answer = logs
        .ok(PUT, &json!({"name": name, "queryString": QUERY_STRING}))
        .await;
    answer["queryDefinitionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a put answers a queryDefinitionId, got {answer}"))
        .to_string()
}

/// The names of the definitions this account holds, as `DescribeQueryDefinitions` reports
/// them.
async fn held(logs: &Logs) -> Vec<String> {
    let page = logs.ok(DESCRIBE, &json!({})).await;
    page["queryDefinitions"]
        .as_array()
        .unwrap_or_else(|| panic!("DescribeQueryDefinitions reports a list, got {page}"))
        .iter()
        .map(|definition| {
            definition["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The message a refusal of `operation` for `id` reports, with the call's HTTP status and
/// error code checked first.
async fn refusal_message(logs: &Logs, case: &str, operation: &str, body: &Value) -> String {
    let (status, parsed) = logs.call(operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must be refused with HTTP 400, got {status} {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        NOT_FOUND,
        "case {case:?}: an id naming no definition is a not-found, got {parsed}"
    );
    parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {parsed}"))
        .to_string()
}

/// A delete removes the one definition its id names, and deleting an id that names none is
/// refused with the delete path's own sentence — for every form such an id takes.
#[tokio::test]
async fn deleting_a_definition_removes_it_and_an_unknown_id_states_the_verdict_alone() {
    let logs = Logs::new();
    let kept = saved(&logs, "kept").await;
    let removed = saved(&logs, "removed").await;

    // The delete removes the definition it names and nothing else.
    let answer = logs
        .ok(DELETE, &json!({"queryDefinitionId": removed}))
        .await;
    assert_eq!(
        answer["success"].as_bool(),
        Some(true),
        "case the stored definition: a delete reports its success, got {answer}"
    );
    assert_eq!(
        held(&logs).await,
        vec!["kept".to_string()],
        "case the stored definition: only the deleted definition is gone"
    );

    // (case, the id) — the id just removed, an id past every one the store has minted (ids
    // come from a monotonic counter, so a node field far beyond it names no definition), and
    // both published bounds of `QueryId` (`@length` 1..256), so the rule is about an id that
    // resolves to no definition rather than about one value.
    let unknown: Vec<(&str, String)> = vec![
        ("the id of the definition just deleted", removed.clone()),
        (
            "a canonical UUID past every id this store minted",
            "00000000-0000-4000-8000-0000000f4240".to_string(),
        ),
        ("an id at the published minimum length", "q".to_string()),
        ("an id at the published maximum length", "q".repeat(256)),
        (
            "the kept definition's id with one character appended",
            format!("{kept}0"),
        ),
    ];

    let mut seen_request_ids: Vec<String> = Vec::new();
    for (case, id) in unknown {
        // The delete states the verdict on the id and nothing else: this call has no other
        // reading of the member to advise about.
        let said = refusal_message(&logs, case, DELETE, &json!({"queryDefinitionId": id})).await;
        let request_id = envelope_request_id(case, &said);
        assert_eq!(
            said,
            format!(
                "{VERDICT} (Service: AWSLogs; Status Code: 400; Error Code: {NOT_FOUND}; \
                 Request ID: {request_id}; Proxy: null)"
            ),
            "case {case:?}: expected the delete path's sentence, got {said:?}"
        );
        // The equality above already pins the whole message, so a value of the request could
        // not have reached it.
        assert!(
            !seen_request_ids.contains(&request_id),
            "case {case:?}: every refusal carries its own request id, got the already-reported \
             {request_id:?}"
        );
        seen_request_ids.push(request_id);

        // The UPDATE path's own sentence for the same id: the verdict, then its guidance.
        let update = json!({
            "queryDefinitionId": id,
            "name": "updated",
            "queryString": QUERY_STRING,
        });
        let said = refusal_message(&logs, case, PUT, &update).await;
        let request_id = envelope_request_id(case, &said);
        assert_eq!(
            said,
            format!(
                "{VERDICT}{UPDATE_GUIDANCE} (Service: AWSLogs; Status Code: 400; Error Code: \
                 {NOT_FOUND}; Request ID: {request_id}; Proxy: null)"
            ),
            "case {case:?}: expected the update path's sentence, got {said:?}"
        );
    }

    // Every refused call left the account's one definition where it was.
    assert_eq!(
        held(&logs).await,
        vec!["kept".to_string()],
        "case after the refusals: a refused call changes nothing"
    );
}
