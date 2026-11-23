// SPDX-License-Identifier: CC0-1.0

//! What `CancelResourceRequest` answers for a `RequestToken` that names no request.
//!
//! The rule every case here holds the service to: **a cancel whose well-formed
//! `RequestToken` names no resource operation request this service holds is a LOOKUP MISS —
//! answered with the modelled `RequestTokenNotFoundException` and its sentence naming the
//! token from the request, under the status this service answers its not-found shapes with
//! (HTTP 400, the status its recorded wire responses carry for every shape the model
//! annotates `@httpError 404`), never the modelled 404 — while a token this service DID mint
//! is served and is left where it was.**
//!
//! The rows differ along how the token that names nothing relates to the store's real
//! tokens: a near miss of a token the service minted (truncated, extended, re-cased, with
//! the token's own punctuation dropped, padded with punctuation the pattern admits, the
//! token written twice), and a token another emulator's store minted — one minted by each of
//! the three operations that mint one, so the rule is about resolving a token rather than
//! about the operation that handed it out. The controls are the tokens this service really
//! minted, which must be served by the same cancel, and the `GetResourceRequestStatus` read
//! of them afterwards, so a refused cancel is shown to change nothing a later caller sees.
//!
//! Token source: `RequestToken` (`@length 1..128`, `@pattern ^[-A-Za-z0-9+/=]+$`), the
//! required input member of `CancelResourceRequest`
//! (`smithy_query(query="operation",operation="CancelResourceRequest")`); every token below
//! satisfies it, so each reaches the handler's lookup rather than the front door's
//! validation.

use serde_json::{json, Value};

use super::emulator::{code, message, progress, text, Emulator};

/// The status this service answers a not-found refusal with.
const NOT_FOUND_STATUS: u16 = 400;

/// The error code the model gives a token that names no request.
const NOT_FOUND_CODE: &str = "RequestTokenNotFoundException";

/// A type whose resources live in this service's own store, so no owning service has to be
/// mounted for a case to mint a request token.
const STREAM: &str = "AWS::Kinesis::Stream";

/// The desired state of the stream called `name`.
fn stream(name: &str) -> Value {
    json!({ "StreamName": name, "ShardCount": 1 })
}

/// A patch that changes nothing: a case here is about the token, not the patch.
fn inert_patch() -> Value {
    json!([{ "op": "add", "path": "/Tags", "value": [] }])
}

/// Check that `(status, body)` is the refusal this service answers a token naming no
/// request with.
fn assert_token_not_found(status: u16, body: &Value, token: &str, case: &str) {
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS} for token {token:?}, got {status} \
         with body {body:?}"
    );
    assert!(
        code(body).contains(NOT_FOUND_CODE),
        "case {case:?}: expected {NOT_FOUND_CODE} for token {token:?}, got {:?}",
        code(body)
    );
    assert_eq!(
        message(body),
        format!("Request with token {token} was not found"),
        "case {case:?}: unexpected wording for token {token:?}"
    );
    assert!(
        progress(body).is_null(),
        "case {case:?}: a refused cancel reports no resource operation, got {body:?}"
    );
}

/// Check that `(status, body)` is a cancel the service SERVED — the control every suite here
/// carries, so what is asserted is a rule about a token that names nothing rather than
/// "every cancel is refused".
fn assert_cancel_served(status: u16, body: &Value, token: &str, case: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: the token {token:?} this service minted must be served, got {status} \
         with body {body:?}"
    );
    assert_eq!(
        text(&progress(body), "RequestToken"),
        token,
        "case {case:?}: expected the cancelled request to report the token it was named by, \
         got {body:?}"
    );
}

/// Every near miss of `token` that still satisfies the `RequestToken` constraints: the rows
/// of the first suite, each paired with what makes it a different token.
fn near_misses(token: &str) -> Vec<(String, String)> {
    let mut shortened = token.to_string();
    shortened.pop();
    vec![
        ("the minted token with its last character dropped", shortened),
        (
            "the minted token with one more character",
            format!("{token}0"),
        ),
        (
            "the minted token in the other letter case",
            token.to_uppercase(),
        ),
        (
            "the minted token with its own punctuation dropped",
            token.replace('-', ""),
        ),
        (
            "the minted token padded with punctuation the pattern admits",
            format!("{token}=="),
        ),
        (
            "the minted token written twice",
            format!("{token}-{token}"),
        ),
    ]
    .into_iter()
    .map(|(row, candidate)| (row.to_string(), candidate))
    .collect()
}

#[tokio::test]
async fn a_near_miss_of_a_minted_token_is_refused_as_a_token_that_names_no_request() {
    let emulator = Emulator::new();
    let accepted = emulator
        .create_ok(STREAM, &stream("cancel-near-miss"), None)
        .await;
    let minted = text(&accepted, "RequestToken");
    assert!(
        !minted.is_empty(),
        "an accepted create must report the token it minted, got {accepted:?}"
    );

    for (row, candidate) in near_misses(&minted) {
        assert_ne!(
            candidate, minted,
            "case {row:?}: the row must name a token other than the minted one"
        );
        assert!(
            (1..=128).contains(&candidate.chars().count())
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-+/=".contains(c)),
            "case {row:?}: the row must satisfy the modelled RequestToken constraints, got \
             {candidate:?}"
        );
        let (status, body) = emulator.cancel(&candidate).await;
        assert_token_not_found(status, &body, &candidate, &row);
    }

    // The control: the token the service really minted is served by the SAME cancel, and the
    // refusals above left it exactly where it was.
    let (status, body) = emulator.status(&minted).await;
    assert_eq!(
        status, 200,
        "the minted token must still resolve after the refusals, got {status} with body \
         {body:?}"
    );
    let (status, body) = emulator.cancel(&minted).await;
    assert_cancel_served(status, &body, &minted, "the minted token");
}

#[tokio::test]
async fn a_token_another_store_minted_names_no_request_here_whichever_operation_minted_it() {
    // Two emulators, each with its own engine: a token one minted names no request in the
    // other, which is what "this service holds no request under that token" means for a
    // token that is otherwise entirely well formed.
    let minting = Emulator::new();
    let other = Emulator::new();

    // One token from each operation that mints one. Each acts on its own resource, so no
    // request waits on another.
    let to_update = minting
        .provision(STREAM, &stream("cancel-cross-store-update"))
        .await;
    let to_delete = minting
        .provision(STREAM, &stream("cancel-cross-store-delete"))
        .await;

    let created = minting
        .create_ok(STREAM, &stream("cancel-cross-store-create"), None)
        .await;
    let (status, body) = minting
        .update(STREAM, &to_update, &inert_patch(), None)
        .await;
    assert_eq!(
        status, 200,
        "UpdateResource must be accepted, got {status} with body {body:?}"
    );
    let updated = progress(&body);
    let (status, body) = minting.delete(STREAM, &to_delete).await;
    assert_eq!(
        status, 200,
        "DeleteResource must be accepted, got {status} with body {body:?}"
    );
    let deleted = progress(&body);

    let rows = [
        ("a token CreateResource minted", text(&created, "RequestToken")),
        ("a token UpdateResource minted", text(&updated, "RequestToken")),
        ("a token DeleteResource minted", text(&deleted, "RequestToken")),
    ];

    for (row, token) in rows {
        // The store that never minted it holds no request under it.
        let (status, body) = other.cancel(&token).await;
        assert_token_not_found(status, &body, &token, row);
        // And the store that DID mint it serves the same call.
        let (status, body) = minting.cancel(&token).await;
        assert_cancel_served(status, &body, &token, row);
    }
}
