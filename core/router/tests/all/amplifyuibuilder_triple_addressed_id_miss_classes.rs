// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal every Amplify UI Builder operation addressed by
//! (`appId`, `environmentName`, `id`) answers when that id does not resolve in the app
//! environment addressed.
//!
//! The RULE: a miss of an id addressed by that triple is classified once, by WHERE the id
//! resolves — an id some OTHER app environment owns is a request made in the wrong scope
//! (`InvalidParameterException`, HTTP 400, one fixed sentence) and an id that resolves
//! nowhere is absent (`ResourceNotFoundException`, HTTP 404) — so every kind and every
//! operation of that kind answer the same class for the same id, and the only part of the
//! answer that belongs to the calling path is the sentence its 404 carries: a delete names
//! the item it could not delete, a read and an update the item they could not find.
//!
//! The rows span what the rule generalises over: the resource KIND addressed (components,
//! forms, codegen jobs and themes — every kind the model addresses by the triple), the
//! OPERATION on it (a read for all four, plus an update and a delete for the three that
//! have them: the ten paths `smithy_query(query="digest")` lists for this service), which
//! half of the (appId, environmentName) pair a wrong-scope row gets wrong, and the FORM of
//! an id that resolves nowhere. The closing censuses are over that table: the ten paths were
//! all exercised, the 404 sentence the three deletes answer is one sentence that no read or
//! update answers, and the reads and updates answer one sentence of their own.
//!
//! The suite lives here rather than in the service crate because the service crate's single
//! integration-test target is a hand-declared `tests/all.rs`, which a fix may not edit; the
//! router's target picks a new suite up with no declaration, and the service answers here
//! exactly as it does in its own crate — it is registered on one engine the way
//! `core/router/src/lib.rs` registers it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "eu-west-1";

/// The app and backend environment that OWNS every resource a row creates.
const APP_ID: &str = "d1234abcd";
const ENVIRONMENT: &str = "staging";

/// The whole sentence a wrong-scope refusal answers, written out here rather than read off
/// the service's own constant so a change of wording is caught rather than followed.
///
/// Source: this service's recorded refusals. The model declares no text for the member
/// (`smithy_query(query="shape",shape="InvalidParameterException")`), so the literal is the
/// one the service is observed to answer.
const WRONG_SCOPE_SENTENCE: &str = "The item requests does not belong to that app or environment";

/// The sentence a REPORTING or UPDATING path answers for an id that resolves nowhere, from
/// the same source as [`WRONG_SCOPE_SENTENCE`].
const ITEM_NOT_FOUND_SENTENCE: &str = "The item specified was not found";

/// The sentence a DELETE answers for an id that resolves nowhere: a deliberately different
/// literal, which is what lets a client tell a failed delete from a failed read apart on an
/// otherwise identical untyped 404.
const ITEM_TO_DELETE_NOT_FOUND_SENTENCE: &str = "Unable to find item to delete";

/// One emulator: a router carrying this service over one shared engine, as
/// `core/router/src/lib.rs` mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/amplifyuibuilder/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000000"
    )
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The resource id the answer carries, or the empty string.
    fn id(&self) -> String {
        self.body["id"].as_str().unwrap_or_default().to_string()
    }

    /// The `message` the answer carries, in either spelling a JSON error body uses, or the
    /// empty string when it carries none.
    fn message(&self) -> &str {
        ["message", "Message"]
            .iter()
            .find_map(|member| self.body[*member].as_str())
            .unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("amplifyuibuilder.{REGION}.amazonaws.com"))
        .header("authorization", authorization());
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// A resource kind this service addresses by the identifier triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Component,
    Form,
    CodegenJob,
    Theme,
}

impl Kind {
    const ALL: [Self; 4] = [Self::Component, Self::Form, Self::CodegenJob, Self::Theme];

    /// The collection element of this kind's REST URIs
    /// (`/app/{appId}/environment/{environmentName}/<collection>/{id}`).
    fn collection(self) -> &'static str {
        match self {
            Self::Component => "components",
            Self::Form => "forms",
            Self::CodegenJob => "codegen-jobs",
            Self::Theme => "themes",
        }
    }

    /// A create payload carrying only the members this kind's create data requires. A codegen
    /// job is started rather than named, so `name` is ignored for it.
    fn create_payload(self, name: &str) -> Value {
        match self {
            Self::Component => json!({
                "name": name,
                "componentType": "View",
                "properties": {},
                "variants": [],
                "overrides": {},
                "bindingProperties": {},
            }),
            Self::Form => json!({
                "name": name,
                "formActionType": "create",
                "dataType": {"dataSourceType": "DataStore", "dataTypeName": "Post"},
                "fields": {},
                "style": {},
                "sectionalElements": {},
                "schemaVersion": "1.0",
            }),
            Self::CodegenJob => json!({"renderConfig": {"react": {}}}),
            Self::Theme => json!({
                "name": name,
                "values": [{"key": "tokens.colors.brand", "value": {"value": "#047d95"}}],
            }),
        }
    }

    /// An update payload this kind's update data accepts, for the kinds that have an update.
    /// `UpdateThemeData$values` is `@required`
    /// (`smithy_query(query="shape",shape="UpdateThemeData")`), so the theme patch carries the
    /// list it is to hold.
    fn update_payload(self) -> Option<Value> {
        match self {
            Self::Component | Self::Form => Some(json!({"name": "Moved"})),
            Self::Theme => Some(json!({
                "name": "Moved",
                "values": [{"key": "tokens.colors.brand", "value": {"value": "#be123c"}}],
            })),
            // The model binds neither an update nor a delete to a codegen job.
            Self::CodegenJob => None,
        }
    }

    /// Every operation on this kind the model addresses by the identifier triple: how it is
    /// called, and the 404 sentence its absent refusal carries.
    fn paths(self) -> Vec<Path> {
        let mut paths = vec![Path {
            operation: "the read",
            method: "GET",
            body: None,
            absent_sentence: ITEM_NOT_FOUND_SENTENCE,
        }];
        if let Some(patch) = self.update_payload() {
            paths.push(Path {
                operation: "the update",
                method: "PATCH",
                body: Some(patch),
                absent_sentence: ITEM_NOT_FOUND_SENTENCE,
            });
            paths.push(Path {
                operation: "the delete",
                method: "DELETE",
                body: None,
                absent_sentence: ITEM_TO_DELETE_NOT_FOUND_SENTENCE,
            });
        }
        paths
    }
}

/// One operation addressed by the triple.
struct Path {
    operation: &'static str,
    method: &'static str,
    body: Option<Value>,
    absent_sentence: &'static str,
}

fn collection_uri(kind: Kind, app_id: &str, environment: &str) -> String {
    format!(
        "/app/{app_id}/environment/{environment}/{}",
        kind.collection()
    )
}

fn member_uri(kind: Kind, app_id: &str, environment: &str, id: &str) -> String {
    format!("{}/{id}", collection_uri(kind, app_id, environment))
}

/// Create (or, for a codegen job, start) one resource of `kind` in the owning scope and
/// report the id it was given.
async fn create(router: &MultiServiceRouter, kind: Kind, name: &str) -> String {
    let answer = call(
        router,
        "POST",
        &collection_uri(kind, APP_ID, ENVIRONMENT),
        Some(kind.create_payload(name)),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "creating a {kind:?} named {name:?} must succeed, got {} with {:?}",
        answer.status, answer.body
    );
    let id = answer.id();
    assert!(
        !id.is_empty(),
        "creating a {kind:?} named {name:?} must report an id, got {:?}",
        answer.body
    );
    id
}

/// The ways an addressed scope can be the wrong one: either half of the pair, or both.
fn wrong_scopes() -> [(&'static str, &'static str, &'static str); 3] {
    [
        ("another app, the same environment", "d9999zzzz", ENVIRONMENT),
        ("the same app, another environment", APP_ID, "production"),
        (
            "another app and another environment",
            "d0mf86cg35xrtv",
            "dev-2",
        ),
    ]
}

/// The forms an id that resolves NOWHERE can arrive in: the id's own shape differs, and
/// every one of them is the same class to the rule.
fn absent_ids() -> [(&'static str, &'static str); 3] {
    [
        (
            "a well-formed uuid nothing was created with",
            "11111111-1111-4111-8111-111111111111",
        ),
        ("an id of one character", "x"),
        ("an id that is not a uuid at all", "not-a-uuid-at-all"),
    ]
}

/// An id that resolves under ANOTHER app environment is refused as wrong-scope — 400 and the
/// one fixed sentence — on every kind and every operation addressed by the triple, and the
/// refusal leaves the resource readable in the scope that owns it.
#[tokio::test]
async fn a_wrong_scope_id_is_the_same_class_on_every_triple_addressed_path() {
    let mut exercised: BTreeSet<(Kind, &'static str)> = BTreeSet::new();

    for kind in Kind::ALL {
        for path in kind.paths() {
            for (scope, app_id, environment) in wrong_scopes() {
                let router = emulator();
                let id = create(&router, kind, "Owned").await;
                let case = format!("{scope} on {} of a {kind:?}", path.operation);

                let answer = call(
                    &router,
                    path.method,
                    &member_uri(kind, app_id, environment, &id),
                    path.body.clone(),
                )
                .await;
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: a wrong-scope id must answer HTTP 400, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.message(),
                    WRONG_SCOPE_SENTENCE,
                    "case {case:?}: expected the message {WRONG_SCOPE_SENTENCE:?}, got {:?}",
                    answer.message()
                );

                // The refusal moved nothing: the resource still reads in its own scope.
                let own = call(
                    &router,
                    "GET",
                    &member_uri(kind, APP_ID, ENVIRONMENT, &id),
                    None,
                )
                .await;
                assert_eq!(
                    own.status, 200,
                    "case {case:?}: the resource must still read in its own scope, got {} with \
                     {:?}",
                    own.status, own.body
                );
                assert_eq!(
                    own.id(),
                    id,
                    "case {case:?}: that read must report the same id, got {:?}",
                    own.body
                );

                exercised.insert((kind, path.method));
            }
        }
    }

    assert_eq!(
        exercised.len(),
        10,
        "case the path census: expected the 10 operations addressed by the identifier triple, \
         exercised {exercised:?}"
    );
}

/// An id that resolves NOWHERE is reported absent — 404 — on every kind and every operation
/// addressed by the triple, and the sentence it carries is the calling path's own: the three
/// deletes answer one sentence of their own, the reads and updates another.
#[tokio::test]
async fn an_absent_id_is_the_same_class_with_the_calling_paths_own_sentence() {
    let mut sentences: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    let mut exercised: BTreeSet<(Kind, &'static str)> = BTreeSet::new();

    for kind in Kind::ALL {
        for path in kind.paths() {
            for (form, absent) in absent_ids() {
                let router = emulator();
                // The addressed scope DOES hold a resource of this kind, so an empty
                // collection is not what decides the answer.
                let live = create(&router, kind, "Owned").await;
                assert_ne!(
                    live, absent,
                    "the absent id {absent:?} must not be the id a create minted"
                );
                let case = format!("{form} on {} of a {kind:?}", path.operation);

                let answer = call(
                    &router,
                    path.method,
                    &member_uri(kind, APP_ID, ENVIRONMENT, absent),
                    path.body.clone(),
                )
                .await;
                assert_eq!(
                    answer.status, 404,
                    "case {case:?}: an id that resolves nowhere must answer HTTP 404, got {} \
                     with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.message(),
                    path.absent_sentence,
                    "case {case:?}: expected the message {:?}, got {:?}",
                    path.absent_sentence,
                    answer.message()
                );
                for value in [APP_ID, ENVIRONMENT, absent] {
                    assert!(
                        !answer.message().contains(value),
                        "case {case:?}: the sentence interpolates no request value, but it \
                         carries {value:?}: {:?}",
                        answer.message()
                    );
                }

                // The live resource of the addressed scope is untouched by the refusal.
                let own = call(
                    &router,
                    "GET",
                    &member_uri(kind, APP_ID, ENVIRONMENT, &live),
                    None,
                )
                .await;
                assert_eq!(
                    own.status, 200,
                    "case {case:?}: the live resource must still read, got {} with {:?}",
                    own.status, own.body
                );

                sentences
                    .entry(path.method)
                    .or_default()
                    .insert(answer.message().to_string());
                exercised.insert((kind, path.method));
            }
        }
    }

    assert_eq!(
        exercised.len(),
        10,
        "case the path census: expected the 10 operations addressed by the identifier triple, \
         exercised {exercised:?}"
    );

    // The reads and the updates answer ONE sentence between them, and the deletes one of
    // their own: the classification is shared, the 404 payload is the calling path's.
    let reporting: BTreeSet<String> = ["GET", "PATCH"]
        .iter()
        .filter_map(|method| sentences.get(*method))
        .flatten()
        .cloned()
        .collect();
    let deleting = sentences.get("DELETE").cloned().unwrap_or_default();
    assert_eq!(
        reporting.len(),
        1,
        "case the reporting wording census: every read and update must answer one sentence, \
         got {reporting:?}"
    );
    assert_eq!(
        deleting.len(),
        1,
        "case the delete wording census: every delete must answer one sentence, got \
         {deleting:?}"
    );
    assert!(
        reporting.is_disjoint(&deleting),
        "case the wording census: a failed delete must not carry the sentence a failed read \
         carries, got {reporting:?} and {deleting:?}"
    );
}
