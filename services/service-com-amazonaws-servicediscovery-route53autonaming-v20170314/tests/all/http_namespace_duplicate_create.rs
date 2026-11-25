// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SECOND `CreateHttpNamespace` for a `Name` a stored HTTP
//! namespace already answers to: the refusal it earns, what that refusal leaves in the
//! store, and when the name becomes creatable again.
//!
//! RULE: an HTTP namespace holds no hosted zone, so its create claims no DNS name and
//! recognises a `Name` another HTTP namespace already answers to as a repeat of the create
//! operation on file for that name — the duplicate-REQUEST class, carrying that operation's
//! id in `DuplicateRequest$DuplicateOperationId` and a sentence naming the operation TYPE
//! that filed it and what became of it. Whatever `CreatorRequestId` the repeat carries (a
//! fresh one each call, as an SDK fills the `@idempotencyToken` in, or none at all), the
//! refusal changes nothing: the name keeps resolving to the namespace that owns it, no
//! second namespace is stored and no operation is filed. The name is refused only while it
//! is IN USE — once the namespace's delete has settled, the identical create is served
//! afresh.
//!
//! The rows differ along what the rule generalises over: the form and length of the `Name`
//! (`NamespaceNameHttp` is modelled `@length 0..1024` with `@pattern
//! ^(?!arn:)[!-~]{1,1024}$`, so every printable non-space ASCII name up to 1024 characters
//! is one the rule covers) and the tokens the two calls carry.
//!
//! A `DuplicateRequest` naming a create that is still RUNNING, the DNS creates' own
//! classification of a held name and the control that the winner's OWN token still replays
//! its operation are `namespace_name_conflict.rs`'s rows, and one delete-frees-the-name row
//! per create kind is `cloud_map_lifecycle.rs`'s. What this suite adds is the `Name` class
//! those suites fix to one form per row, and the reads proving the refusal filed no
//! operation and stored no namespace.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::async_namespace_operations::{deleted_namespace, set_operation_due};
use crate::cloud_map_core::{all_page_entries, call, emulator_with_engine, entry_ids, ok};

/// The create under test, and the operation TYPE name its refusal reports.
const HTTP_CREATE: &str = "CreateHttpNamespace";

/// The error the model declares for a request that repeats an operation of its own.
const DUPLICATE_REQUEST: &str = "DuplicateRequest";

/// The ONE status every client-class refusal of this service answers, whatever
/// `@httpError` the model pins on the shape (`client_error_wire_shape.rs` states that
/// rule and spans the classes it covers).
const CLIENT_STATUS: u16 = 400;

/// The published maximum of a listing filter's value (`FilterValue`, `@length 1..255`), so
/// a name longer than this cannot be sent as a `NAME` filter value at all.
const FILTER_VALUE_MAX: usize = 255;

/// The error class one refused answer names, read from the awsJson envelope's `__type` and
/// unqualified, so a row compares the class exactly rather than by suffix.
fn error_class(answer: &Value) -> String {
    answer["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// One row's `CreateHttpNamespace` body: the `Name`, the `CreatorRequestId` when the call
/// carries one, and the tags the recorded sequence sends with its first create.
fn create_body(name: &str, token: Option<&str>, tagged: bool) -> Value {
    let mut body = json!({ "Name": name });
    if let Some(token) = token {
        body["CreatorRequestId"] = json!(token);
    }
    if tagged {
        body["Tags"] = json!([{ "Key": "owner", "Value": "duplicate-create-suite" }]);
    }
    body
}

/// Create an HTTP namespace and answer `(the create's OperationId, the namespace id)` once
/// that operation has settled.
///
/// The operation's due instant is MOVED rather than waited for, so the sequence below runs
/// without depending on how fast the host runs.
async fn settled_http_namespace(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    body: Value,
) -> (String, String) {
    let created = ok(router, HTTP_CREATE, body).await;
    let operation_id = created["OperationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {HTTP_CREATE} answers an OperationId, got {created}"))
        .to_string();
    set_operation_due(engine, &operation_id, true);
    let filed = ok(router, "GetOperation", json!({ "OperationId": &operation_id })).await;
    assert_eq!(
        filed["Operation"]["Status"],
        json!("SUCCESS"),
        "case {case:?}: the create the clock owes must report SUCCESS, got {filed}"
    );
    let namespace_id = filed["Operation"]["Targets"]["NAMESPACE"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the CREATE_NAMESPACE operation targets its namespace, got {filed}")
        })
        .to_string();
    (operation_id, namespace_id)
}

/// The ids of the namespaces that answer to `name`, read back through `ListNamespaces`.
///
/// A name within the published `FilterValue` maximum is asked for with the `NAME`/`EQ`
/// filter, which is how a caller asks this question; a longer one is read from the
/// unfiltered listing, because such a value cannot be sent as a filter value at all.
async fn namespaces_holding(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let body = if name.len() <= FILTER_VALUE_MAX {
        json!({ "Filters": [{ "Name": "NAME", "Values": [name], "Condition": "EQ" }] })
    } else {
        json!({})
    };
    let listed = all_page_entries(router, case, "ListNamespaces", "Namespaces", body).await;
    listed
        .iter()
        .filter(|summary| summary["Name"] == json!(name))
        .map(|summary| {
            summary["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: every summary names its namespace, got {summary}"))
                .to_string()
        })
        .collect()
}

/// Every operation this service currently holds a record of.
async fn operations_on_file(router: &MultiServiceRouter, case: &str) -> BTreeSet<String> {
    let listed = all_page_entries(router, case, "ListOperations", "Operations", json!({})).await;
    entry_ids(case, &listed)
}

/// The names spanning the rule: each is a `Name` the modelled pattern admits, and the
/// 1024-character row is the published maximum.
///
/// That pattern admits the ASCII range `!`..`~` only, so every stem is ASCII and a row may
/// slice one by bytes to make it unique.
fn names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name", "dup-plain-ns".to_string()),
        ("a single-character name", "x".to_string()),
        (
            "a name that reads as a DNS name",
            "dup.plain.ns.example.com".to_string(),
        ),
        (
            "a name carrying the printable punctuation the pattern admits",
            "dup!ns~#1/9:0@x-y_z+(a)[b]{c}".to_string(),
        ),
        (
            "a name at the published filter-value maximum",
            format!("dup-{}", "n".repeat(FILTER_VALUE_MAX - 4)),
        ),
        (
            "a name at the published 1024-character maximum",
            format!("dup-{}", "m".repeat(1_020)),
        ),
    ]
}

/// RULE: a second `CreateHttpNamespace` for a name a stored HTTP namespace answers to is
/// refused as the duplicate request naming the create operation that owns the name — over
/// every name form and over the tokens the two calls carry — and the refusal stores
/// nothing: the name still resolves to exactly the original namespace, that namespace reads
/// back unchanged, and no operation was filed for the refused call.
#[tokio::test]
async fn a_duplicate_http_namespace_create_is_refused_naming_the_create_that_owns_the_name() {
    let (router, engine) = emulator_with_engine();

    // (the winner's CreatorRequestId, the repeat's) — an SDK fills the modelled
    // `@idempotencyToken` with a FRESH value on every call, so the repeat carrying a token
    // of its own is the common case and not a curiosity.
    let tokens: [(&str, Option<&str>, Option<&str>); 3] = [
        ("neither call carries a token", None, None),
        ("only the repeat carries one", None, Some("repeat-only")),
        ("each call carries its own", Some("winner-own"), Some("repeat-own")),
    ];

    for (index, (tokens_case, winner_token, repeat_token)) in tokens.into_iter().enumerate() {
        for (name_case, stem) in names() {
            let case = format!("{name_case}, {tokens_case}");
            // Each row needs its own name, and the long rows are already at their
            // published maximum, so the row index replaces the stem's first characters
            // instead of being appended to it.
            let name = format!("{index}{}", &stem[1..]);

            let (filed, owner) = settled_http_namespace(
                &router,
                &engine,
                &case,
                create_body(&name, winner_token, true),
            )
            .await;
            let read_before = ok(&router, "GetNamespace", json!({ "Id": &owner })).await;
            let filed_before = operations_on_file(&router, &case).await;

            let (status, answer) =
                call(&router, HTTP_CREATE, create_body(&name, repeat_token, false)).await;
            assert_eq!(
                (status, error_class(&answer).as_str()),
                (CLIENT_STATUS, DUPLICATE_REQUEST),
                "case {case:?}: the repeat of the create that owns the name must be refused \
                 as the duplicate request, got {status} {answer}"
            );
            assert_eq!(
                answer["DuplicateOperationId"],
                json!(filed),
                "case {case:?}: the refusal must name the create operation on file for the \
                 name, got {answer}"
            );
            assert_eq!(
                answer["Message"],
                json!(format!(
                    "Another operation of type {HTTP_CREATE} and id {filed} has completed"
                )),
                "case {case:?}: the refusal must name the operation type and id of the \
                 settled create, got {answer}"
            );

            assert_eq!(
                namespaces_holding(&router, &case, &name).await,
                vec![owner.clone()],
                "case {case:?}: exactly the original namespace must still answer to the name"
            );
            let read_after = ok(&router, "GetNamespace", json!({ "Id": &owner })).await;
            assert_eq!(
                read_after["Namespace"], read_before["Namespace"],
                "case {case:?}: the refusal must leave the stored namespace untouched, got \
                 {read_after} after {read_before}"
            );
            assert_eq!(
                operations_on_file(&router, &case).await,
                filed_before,
                "case {case:?}: the refused create must file no operation"
            );
        }
    }
}

/// RULE, the other side of it: the name is refused only while it is IN USE, so once the
/// namespace's delete has settled the identical create is served afresh — a new namespace,
/// which reads back under that same name.
#[tokio::test]
async fn deleting_the_namespace_frees_the_name_for_the_identical_http_create() {
    let (router, engine) = emulator_with_engine();

    // The repeated call carries the winner's OWN token in one row: once the namespace is
    // gone the token is stale, so that call is a fresh create rather than a replay of the
    // operation of a namespace nothing holds any more.
    for (index, token) in [None, Some("freed-name-token")].into_iter().enumerate() {
        for (name_case, stem) in names() {
            let case = format!("{name_case}, token {token:?}");
            let name = format!("{index}{}", &stem[1..]);

            let (_, owner) =
                settled_http_namespace(&router, &engine, &case, create_body(&name, token, false))
                    .await;

            let deleted = ok(&router, "DeleteNamespace", json!({ "Id": &owner })).await;
            let delete_operation = deleted["OperationId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: DeleteNamespace answers an OperationId, got {deleted}"))
                .to_string();
            set_operation_due(&engine, &delete_operation, true);
            deleted_namespace(&router, &case, &owner).await;

            let (_, recreated) =
                settled_http_namespace(&router, &engine, &case, create_body(&name, token, false))
                    .await;
            assert_ne!(
                recreated, owner,
                "case {case:?}: the freed name must be created afresh, not replayed"
            );
            let read = ok(&router, "GetNamespace", json!({ "Id": &recreated })).await;
            assert_eq!(
                read["Namespace"]["Name"],
                json!(name),
                "case {case:?}: the re-created namespace must answer to the freed name, got \
                 {read}"
            );
            assert_eq!(
                namespaces_holding(&router, &case, &name).await,
                vec![recreated.clone()],
                "case {case:?}: only the re-created namespace must answer to the name"
            );
        }
    }
}
