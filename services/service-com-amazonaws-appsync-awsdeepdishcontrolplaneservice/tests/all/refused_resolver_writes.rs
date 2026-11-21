// SPDX-License-Identifier: CC0-1.0

//! What a REFUSED resolver write leaves behind, read through `ListResolvers`.
//!
//! The rule: a resolver exists, and carries the definition it carries, only because a call
//! that was SERVED wrote it — so a refused `CreateResolver` or `UpdateResolver` writes
//! nothing, and the listing of the type reports exactly the definitions that were
//! accepted: the previously accepted body for a field that already carries a resolver, and
//! no entry at all for a field whose only write was refused. The listing is the reader
//! asserted here (its sibling suites read one resolver through `GetResolver`), because the
//! state divergence outlives the response divergence: a client that only inventories a
//! type sees it.
//!
//! The rows span the refusal CLASSES a resolver write takes, each one a different reason
//! the call is refused rather than a different value of one reason: `code` the APPSYNC_JS
//! runtime cannot read, a caching key rooted outside the context maps, a caching TTL
//! outside the documented window, a sync configuration naming the Lambda conflict handler
//! without its function, and one naming a function of no account. Every assertion goes
//! through the router the production emulator mounts; the API harness is the resolver
//! suites' own, reused rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router, Answer};
use super::resolvers::{api_with_dependencies, create_resolver_body, DATA_SOURCE, TYPE};

/// The APPSYNC_JS runtime a definition carrying `code` names. Source:
/// `smithy_query(query="shape", shape="AppSyncRuntime")`.
const RUNTIME: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The code the accepted resolver is defined by, which every refused write below must
/// leave in place.
const ACCEPTED_CODE: &str = "export function request(ctx) { return { payload: ctx.args }; }\n";

/// One refusal class: the members a write carries beside the definition, and what the
/// refusal is about.
struct Row {
    /// What the row exercises, named in every assertion of it.
    label: &'static str,
    /// The members this row adds to a write of an otherwise valid APPSYNC_JS resolver.
    refused: fn() -> Value,
}

/// The refusal classes a resolver write takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "code the runtime cannot read",
            refused: || {
                json!({
                    "code": "export function request(ctx) {\n  return {\n    payload: ctx.args,\n",
                })
            },
        },
        Row {
            label: "a caching key rooted outside the context maps",
            refused: || json!({"cachingConfig": {"ttl": 60, "cachingKeys": ["$context.nope.id"]}}),
        },
        Row {
            label: "a caching TTL outside the documented window",
            refused: || json!({"cachingConfig": {"ttl": 3_601}}),
        },
        Row {
            label: "the lambda conflict handler supplying no function",
            refused: || {
                json!({"syncConfig": {"conflictDetection": "VERSION", "conflictHandler": "LAMBDA"}})
            },
        },
        Row {
            label: "a conflict handler naming a function of no account",
            refused: || {
                json!({
                    "syncConfig": {
                        "conflictDetection": "VERSION",
                        "conflictHandler": "LAMBDA",
                        "lambdaConflictHandlerConfig": {
                            "lambdaConflictHandlerArn": "resolve_conflicts",
                        },
                    },
                })
            },
        },
    ]
}

/// A write of the resolver on `field`, defined by the accepted code and whatever `extra`
/// adds to it. An update addresses the field in its URI rather than in the body, so a
/// write that names no field carries none.
fn resolver_body(field: Option<&str>, extra: Value) -> Value {
    let mut body = json!({
        "dataSourceName": DATA_SOURCE,
        "code": ACCEPTED_CODE,
        "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
    });
    let object = body.as_object_mut().expect("the body is a JSON object");
    if let Some(field) = field {
        object.insert("fieldName".to_string(), json!(field));
    }
    for (member, value) in extra.as_object().expect("the row's members are an object") {
        object.insert(member.clone(), value.clone());
    }
    body
}

/// The entry the type's listing reports for `field`, or `None` when it reports none.
async fn listed(router: &MultiServiceRouter, api_id: &str, field: &str) -> Option<Value> {
    let answer = call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case list the resolvers of {TYPE}: expected 200, got {answer:?}"
    );
    answer.body["resolvers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|resolver| resolver["fieldName"] == json!(field))
}

/// Assert `answer` is a 400 `BadRequestException`, whatever the sentence it carries: what
/// this suite reads is the STATE a refusal leaves, and the sentences themselves are the
/// sibling suites' subject.
fn assert_refused(case: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, "BadRequestException"),
        "case {case}: expected 400 BadRequestException, got {answer:?}"
    );
}

#[tokio::test]
async fn a_refused_create_leaves_the_listing_without_the_field() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_refused_writes_create").await;

    for (index, row) in rows().iter().enumerate() {
        let field = format!("refused_created_{index}");
        let created = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            resolver_body(Some(&field), (row.refused)()),
        )
        .await;
        assert_refused(row.label, &created);

        let entry = listed(&router, &api_id, &field).await;
        assert!(
            entry.is_none(),
            "case {}: expected the listing to report no resolver on {field}, got {entry:?}",
            row.label
        );
    }
}

#[tokio::test]
async fn a_refused_write_leaves_the_accepted_definition_in_the_listing() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_refused_writes_keep").await;

    for (index, row) in rows().iter().enumerate() {
        let field = format!("refused_kept_{index}");

        // The definition AppSync accepted, which every refusal below must leave in place.
        let accepted = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            resolver_body(Some(&field), json!({})),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {}: expected the resolver to attach first, got {accepted:?}",
            row.label
        );
        let stored = listed(&router, &api_id, &field).await;
        assert_eq!(
            stored.as_ref().map(|entry| entry["code"].clone()),
            Some(json!(ACCEPTED_CODE)),
            "case {}: expected the listing to report the accepted code, got {stored:?}",
            row.label
        );

        // A create on the field that already carries a resolver is refused, and so is an
        // update that would replace it with the row's definition — and neither writes.
        let recreated = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            resolver_body(Some(&field), (row.refused)()),
        )
        .await;
        assert_refused(&format!("re-create {}", row.label), &recreated);

        let updated = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            Some(resolver_body(None, (row.refused)())),
        )
        .await;
        assert_refused(&format!("update {}", row.label), &updated);

        let after = listed(&router, &api_id, &field).await;
        assert_eq!(
            after, stored,
            "case {}: expected the listing to report the accepted resolver {stored:?}, got \
             {after:?}",
            row.label
        );
    }
}
