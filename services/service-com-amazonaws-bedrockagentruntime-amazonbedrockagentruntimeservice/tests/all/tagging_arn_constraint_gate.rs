// SPDX-License-Identifier: CC0-1.0

//! WHEN the `resourceArn` of a tagging operation is checked, and against WHICH of its
//! modelled constraints.
//!
//! RULE: `resourceArn` is an `@httpLabel` targeting `TaggableResourcesArn`, which carries
//! `@length(20, 1011)` and a `@pattern`
//! (`smithy_query(query="operation",operation="TagResource,UntagResource,ListTagsForResource")`
//! -> `resourceArn`, and `smithy_query(query="shape",shape="TaggableResourcesArn")`), so a
//! value outside EITHER constraint is refused as INPUT — the modelled
//! `ValidationException` (400) carrying the constraint-violation envelope, one clause per
//! requirement broken — BEFORE the value is resolved against any state. The store is
//! therefore neither read nor written for such a request, whatever it would have resolved
//! to, and the refusal is the same on all three identically-bound tagging routes.
//!
//! The two suites below take the two halves of that rule:
//!
//! * the ORDER — a label the store would resolve (a session's own bare id, which every
//!   session-scoped operation of this service accepts for the same session) is refused by
//!   the gate instead, and the session it names keeps the tags it had;
//! * the CONSTRAINT CLASS — the `@length` bounds are enforced beside the `@pattern`, each
//!   published bound admitted and the value one character past it refused, with a clause
//!   per requirement in one envelope.
//!
//! What a `@pattern` clause QUOTES is `tests/all/pattern_constraint_messages.rs`'s rule,
//! and which `/tags/{resourceArn}` requests this service answers at all is
//! `tests/all/shared_tagging_arn_claim.rs`'s; both are read here, not restated.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sessions::{call, error_code, label, router};

/// The status and error code this model declares for an input-constraint refusal:
/// `ValidationException`, `httpError` 400.
const INVALID: (u16, &str) = (400, "ValidationException");

/// The inclusive bounds `TaggableResourcesArn` declares
/// (`smithy_query(query="shape",shape="TaggableResourcesArn")` -> `length {min: 20, max:
/// 1011}`).
const MIN_LENGTH: usize = 20;
const MAX_LENGTH: usize = 1011;

/// The expression a `resourceArn` clause quotes: the `TaggableResourcesArn` this service
/// PUBLISHES, stated as the expression the whole value must match (this crate's
/// `service::PUBLISHED_PATTERNS`; `tests/all/pattern_constraint_messages.rs` owns that
/// rule).
const PUBLISHED_TAGGABLE_ARN: &str = "arn:aws(-[^:]+)?:bedrock:[a-zA-Z0-9-]+:[0-9]{12}\
                                      :(session|advanced-optimize-prompt-job)/[a-zA-Z0-9-]+";

/// The `message` member of an error body, or `""` when there is none.
fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// One tagging operation: how it is addressed, and the members its own model requires
/// beside `resourceArn`, so the only thing a refusal can be about is the ARN.
struct Operation {
    name: &'static str,
    method: &'static str,
    /// The path for one `resourceArn`, label-encoded as an SDK sends a non-greedy label.
    path_of: fn(&str) -> String,
    /// The request body, `Value::Null` for an operation that carries none.
    body: Value,
}

/// The three routes this model binds on `/tags/{resourceArn}`
/// (`POST`, `DELETE` and `GET`), each with the rest of its required input.
fn tagging_operations() -> Vec<Operation> {
    vec![
        Operation {
            name: "TagResource",
            method: "POST",
            path_of: |arn| format!("/tags/{}", label(arn)),
            body: json!({ "tags": { "gate": "probe" } }),
        },
        Operation {
            name: "UntagResource",
            method: "DELETE",
            path_of: |arn| format!("/tags/{}?tagKeys=keep", label(arn)),
            body: Value::Null,
        },
        Operation {
            name: "ListTagsForResource",
            method: "GET",
            path_of: |arn| format!("/tags/{}", label(arn)),
            body: Value::Null,
        },
    ]
}

/// The clause stated about `resourceArn`: the value as the request submitted it, the
/// member, and the requirement it broke.
fn clause(value: &str, requirement: &str) -> String {
    format!("Value '{value}' at 'resourceArn' failed to satisfy constraint: {requirement}")
}

/// The requirement clause of the `@pattern` class.
fn pattern_requirement() -> String {
    format!("Member must satisfy regular expression pattern: {PUBLISHED_TAGGABLE_ARN}")
}

/// The counting envelope over `clauses`, in the framework's own shape.
fn envelope(clauses: &[String]) -> String {
    match clauses.len() {
        1 => format!("1 validation error detected: {}", clauses[0]),
        n => format!("{n} validation errors detected: {}", clauses.join("; ")),
    }
}

/// The value as a clause ECHOES it: itself, or a prefix with the byte count once it is
/// longer than the framework's echo cap (`MAX_ECHOED_VALUE` in
/// `vendor/smithy-schema-server/src/constraints.rs`), so one oversized member cannot
/// amplify a request into a far larger message.
fn echoed(value: &str) -> String {
    const CAP: usize = 256;
    if value.len() <= CAP {
        return value.to_string();
    }
    format!("{}... [{} bytes]", &value[..CAP], value.len())
}

/// Create a session carrying `tags` and return `(sessionId, sessionArn)`.
async fn create_tagged_session(router: &MultiServiceRouter, tags: Value) -> (String, String) {
    let (status, out) = call(router, "PUT", "/sessions/", json!({ "tags": tags })).await;
    assert_eq!(
        status, 201,
        "CreateSession: expected 201, got {status} {out:?}"
    );
    (
        out["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateSession: no sessionId in {out:?}"))
            .to_string(),
        out["sessionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateSession: no sessionArn in {out:?}"))
            .to_string(),
    )
}

/// The tags `ListTagsForResource` reports for an ARN the gate admits.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Value {
    let (status, out) = call(router, "GET", &format!("/tags/{}", label(arn)), Value::Null).await;
    assert_eq!(
        status, 200,
        "ListTagsForResource({arn}): expected 200, got {status} {out:?}"
    );
    out.get("tags").cloned().unwrap_or(Value::Null)
}

/// RULE, first half: the gate runs AHEAD of the lookup, so a label this service's own
/// session resolution WOULD resolve is refused as input rather than served — and the
/// session it names is neither read nor written.
///
/// The rows are the forms of a `sessionIdentifier` this service accepts everywhere a
/// session is addressed (`crate::state::session_id_from_identifier`: the bare id, and the
/// session ARN) but which `TaggableResourcesArn` does not admit, taken from a session that
/// really is in the store: its bare id, that id in the upper case an ARN's resource field
/// does not carry, and the ARN with its `arn:` prefix dropped. Each is long enough to
/// clear the `@length` minimum, so the one clause is the `@pattern`.
#[tokio::test]
async fn the_resource_arn_gate_refuses_before_the_session_store_is_reached() {
    let router = router();
    let stored = json!({ "keep": "me" });
    let (session_id, session_arn) = create_tagged_session(&router, stored.clone()).await;

    // The same store address, in the ARN form the gate admits: served, which is what makes
    // the refusals below about the gate and not about the session being absent.
    assert_eq!(
        list_tags(&router, &session_arn).await,
        stored,
        "the session's tags must be served through the ARN form the gate admits"
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "the session's own bare id, which every session-scoped operation accepts",
            session_id.clone(),
        ),
        (
            "the same id upper-cased",
            session_id.to_uppercase(),
        ),
        (
            "the session's ARN with its arn prefix dropped",
            session_arn
                .strip_prefix("arn:")
                .expect("a minted sessionArn opens with the ARN prefix")
                .to_string(),
        ),
    ];

    for operation in &tagging_operations() {
        for (form, identifier) in &rows {
            let case = format!("{} addressed by {form}", operation.name);
            assert!(
                identifier.len() >= MIN_LENGTH,
                "case {case:?}: the row must clear the length minimum, but {identifier:?} is \
                 {} characters",
                identifier.len()
            );
            let (status, refused) = call(
                &router,
                operation.method,
                &(operation.path_of)(identifier),
                operation.body.clone(),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                INVALID,
                "case {case:?}: expected {INVALID:?} for {identifier:?}, got {status} \
                 {refused:?}"
            );
            let expected = envelope(&[clause(identifier, &pattern_requirement())]);
            let stated = message(&refused);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected message {expected:?}, got {stated:?}"
            );
            // A refused read reports nothing of the resource either: no `tags` member
            // travels with the refusal.
            assert!(
                refused.get("tags").is_none(),
                "case {case:?}: a refused request must report no resource state, got {refused:?}"
            );
        }
    }

    // Neither the tag write nor the untag above reached the store: the session still
    // carries exactly the map it was created with.
    assert_eq!(
        list_tags(&router, &session_arn).await,
        stored,
        "the stored session's tags must be untouched by every refused request"
    );
}

/// One length row: a `resourceArn` of a chosen length, and the requirements it breaks.
struct LengthRow {
    case: &'static str,
    /// The number of characters the submitted label carries.
    length: usize,
    /// Whether the `@length` minimum is broken, the maximum is broken, or neither.
    bound: Option<&'static str>,
}

/// RULE, second half: the `@length` bounds `TaggableResourcesArn` declares are enforced
/// beside its `@pattern` — a value at either published bound breaks only the pattern, and
/// one character past a bound breaks that bound too, stated as its own clause in the one
/// counting envelope.
///
/// The rows span the range the rule generalises over: each published bound, one character
/// short of the minimum, and one character past the maximum. The value is filler no ARN
/// grammar admits, so every row breaks the pattern and the only thing differing between
/// them is the length.
#[tokio::test]
async fn the_resource_arn_length_bounds_are_enforced_beside_the_pattern() {
    let router = router();

    let rows: [LengthRow; 4] = [
        LengthRow {
            case: "one character short of the published minimum",
            length: MIN_LENGTH - 1,
            bound: Some("min"),
        },
        LengthRow {
            case: "exactly the published minimum",
            length: MIN_LENGTH,
            bound: None,
        },
        LengthRow {
            case: "exactly the published maximum",
            length: MAX_LENGTH,
            bound: None,
        },
        LengthRow {
            case: "one character past the published maximum",
            length: MAX_LENGTH + 1,
            bound: Some("max"),
        },
    ];

    for operation in &tagging_operations() {
        for row in &rows {
            let case = format!("{} with a resourceArn {}", operation.name, row.case);
            let value = "a".repeat(row.length);
            let echo = echoed(&value);
            let mut clauses = Vec::new();
            // The framework states the length clause before the pattern one, which is the
            // order the two constraints are checked in.
            match row.bound {
                Some("min") => clauses.push(clause(
                    &echo,
                    &format!("Member must have length greater than or equal to {MIN_LENGTH}"),
                )),
                Some("max") => clauses.push(clause(
                    &echo,
                    &format!("Member must have length less than or equal to {MAX_LENGTH}"),
                )),
                _ => {}
            }
            clauses.push(clause(&echo, &pattern_requirement()));

            let (status, refused) = call(
                &router,
                operation.method,
                &(operation.path_of)(&value),
                operation.body.clone(),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                INVALID,
                "case {case:?}: expected {INVALID:?} for a {}-character value, got {status} \
                 {refused:?}",
                row.length
            );
            let expected = envelope(&clauses);
            let stated = message(&refused);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected message {expected:?}, got {stated:?}"
            );
        }
    }
}
