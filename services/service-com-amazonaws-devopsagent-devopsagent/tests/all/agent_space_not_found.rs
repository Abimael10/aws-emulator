// SPDX-License-Identifier: CC0-1.0

//! The wordings this service answers an `agentSpaceId` that names no AgentSpace
//! with, and the identifiers that name none — shared by every suite that drives
//! such a lookup.
//!
//! The rule it encodes: a not-found refusal is worded after the resource the
//! operation actually resolved. An operation whose target resource IS the
//! AgentSpace (`GetAgentSpace`, `DeleteAgentSpace` on
//! `/v1/agentspaces/{agentSpaceId}`) names that resource, the member its
//! identifier arrived on and the value itself — unquoted, with a terminal period
//! and no account clause. An Operator App operation resolves a resource INSIDE
//! the space, and its missing-parent refusal names both the identifier the
//! request carried and the account the lookup ran in, each double-quoted. Both
//! are HTTP 404 `ResourceNotFoundException`, computed from the request and the
//! store rather than from a literal.
//!
//! This module holds no test of its own: it is the single copy of those
//! expectations, so a suite for one slice of the service (the Operator App, the
//! AgentSpace lifecycle) asserts the same text as every other and the text cannot
//! drift between them.

use serde_json::Value;

/// The published bounds of `AgentSpaceId`'s `^[a-zA-Z0-9-]{1,64}$`, from
/// `smithy_query(query="shape", shape="AgentSpaceId")`.
pub(crate) const AGENT_SPACE_ID_MIN_LEN: usize = 1;
pub(crate) const AGENT_SPACE_ID_MAX_LEN: usize = 64;

/// The account every request this emulator serves is signed for, which is the account
/// scope an AgentSpace lookup runs in.
pub(crate) fn caller_account() -> &'static str {
    ::emulator_server::emulator_account_id()
}

/// The whole wording an operation addressed to the AgentSpace itself refuses a
/// missing identifier with: fixed text naming the resource and the member the
/// identifier arrived on, the requested value verbatim and unquoted, and a
/// terminal period. Written out here rather than reused from the service so the
/// tests pin the text itself.
pub(crate) fn agent_space_expected_message(agent_space_id: &str) -> String {
    format!("AgentSpace with agentSpaceId {agent_space_id} not found.")
}

/// The whole wording an Operator App operation refuses a request naming no
/// AgentSpace with: fixed text around two double-quoted slots, the requested
/// identifier and the account the lookup ran in.
pub(crate) fn operator_app_expected_message(agent_space_id: &str) -> String {
    format!(
        "Could not find Agent Space with provided agentSpaceId \"{agent_space_id}\" in accountId \"{}\"",
        caller_account()
    )
}

/// Identifiers that satisfy `AgentSpaceId`'s pattern and name no stored AgentSpace,
/// differing in length, character class and form across the whole of it: the shortest
/// admissible one, a digit-only one, one in the form this service mints, a mixed-case
/// hyphenated one, an all-hyphen one, and one at the pattern's maximum length.
pub(crate) fn missing_identifiers() -> Vec<String> {
    vec![
        "a".repeat(AGENT_SPACE_ID_MIN_LEN),
        "7".to_string(),
        "as-0000000000000000".to_string(),
        "Mixed-Case-Agent-Space-42".to_string(),
        "---".to_string(),
        "z".repeat(AGENT_SPACE_ID_MAX_LEN),
    ]
}

/// The wire error code (`__type`) of an error response.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Assert one call's refusal: HTTP 404, `ResourceNotFoundException`, and exactly
/// `expected` as the message.
pub(crate) fn assert_not_found(case: &str, expected: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected 404 for an agentSpaceId naming no AgentSpace, got {status} ({body})"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {body}"
    );
    let actual = body["message"].as_str().unwrap_or("<no message>");
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the not-found wording {expected:?}, got {actual:?}"
    );
}

/// Assert the refusal of an Operator App operation whose parent AgentSpace is
/// absent.
pub(crate) fn assert_operator_app_space_not_found(
    case: &str,
    agent_space_id: &str,
    status: u16,
    body: &Value,
) {
    assert_not_found(
        case,
        &operator_app_expected_message(agent_space_id),
        status,
        body,
    );
}
