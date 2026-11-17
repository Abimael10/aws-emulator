// SPDX-License-Identifier: CC0-1.0

//! `DeleteDataLakeNamespace` driven over the wire (restJson1, signing name `scn`) through
//! the same `MultiServiceRouter` the emulator's front door uses — so every assertion here
//! is about what a real client observes, not about a handler's internals.
//!
//! THE RULE UNDER TEST: a data lake namespace is removable only while it holds no
//! dataset. While any dataset of that instance lives under the namespace, the delete is
//! refused `ValidationException` (400) with the sentence naming the namespace and leaves
//! the namespace and its datasets exactly as they were; once the namespace is empty the
//! delete removes it and answers with the instance and name it removed, after which the
//! namespace is unaddressable — a read of it and a repeat delete both answer
//! `ResourceNotFoundException` (404) with the service's "Namespace <name> not exist". A
//! namespace the instance comes with is never a caller's to delete, and an `instanceId`
//! naming no instance of this account is refused by the gateway's authorization step
//! before any namespace is looked up.
//!
//! The tables span what the rule generalises over: the `DataLakeNamespaceName` form and
//! length (both published bounds), how many datasets block the delete, which namespace
//! holds them (the addressed one, a sibling, a pre-defined one), and which namespace name
//! the delete addresses.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::PREDEFINED_NAMESPACES;
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for.
const REGION: &str = "us-east-1";

/// The sentence a namespace operation refuses a name the data lake does not hold with,
/// written out HERE rather than read from the crate: the wording is the requirement, so a
/// drift in the function the handlers answer from fails this suite instead of travelling
/// with it. Its SOURCE is the recorded wire response of a read and of a delete addressed
/// to a namespace that was removed.
fn not_found_sentence(name: &str) -> String {
    format!("Namespace {name} not exist")
}

/// The sentence a delete of a namespace that still holds datasets is refused with. Its
/// SOURCE is the recorded wire response of such a delete: the namespace name is the only
/// part taken from the request.
fn not_empty_sentence(name: &str) -> String {
    format!("Datasets found under the namespace {name}. Please delete them before deleting the namespace")
}

/// The sentence a delete addressed to a namespace the instance comes with is refused
/// with; its SOURCE is the `name` member of `DeleteDataLakeNamespaceRequest` — "Noted you
/// cannot delete pre-defined namespace like asc, default which are only deleted through
/// instance deletion" — and the namespace the request addressed is its only variable part.
fn predefined_sentence(name: &str) -> String {
    format!("Cannot delete pre-defined namespace {name}")
}

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can read the store back and
    /// assert what a refused call left behind as well as what it reported.
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// How many namespace records the store holds for `instance_id`.
    fn stored_namespaces(&self, instance_id: &str) -> usize {
        service_com_amazonaws_supplychain_galaxypublicapigateway::state::service_state(
            &*self.engine,
        )
        .expect("the supplychain store is reachable on this engine")
        .lock()
        .namespace_count(instance_id)
    }

    /// How many dataset records the store holds for `instance_id`.
    fn stored_datasets(&self, instance_id: &str) -> usize {
        service_com_amazonaws_supplychain_galaxypublicapigateway::state::service_state(
            &*self.engine,
        )
        .expect("the supplychain store is reachable on this engine")
        .lock()
        .dataset_count(instance_id)
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("scn.{REGION}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A live instance of this account, created through `CreateInstance`.
    async fn given_instance(&self, case: &str) -> String {
        let (status, body) = self.send("POST", "/api/instance", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// `CreateDataLakeNamespace` for `name`, asserted to succeed.
    async fn given_namespace(&self, case: &str, instance_id: &str, name: &str) {
        let (status, body) = self
            .send("PUT", &namespace_path(instance_id, name), Some(json!({})))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateDataLakeNamespace expected 200, got {status} with {body}"
        );
    }

    /// `CreateDataLakeDataset` for one triple, asserted to succeed.
    async fn given_dataset(&self, case: &str, instance_id: &str, namespace: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &dataset_path(instance_id, namespace, name),
                Some(json!({ "schema": minimal_schema() })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
    }

    async fn delete_namespace(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("DELETE", &namespace_path(instance_id, name), None)
            .await
    }

    async fn get_namespace(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("GET", &namespace_path(instance_id, name), None)
            .await
    }

    async fn delete_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send("DELETE", &dataset_path(instance_id, namespace, name), None)
            .await
    }

    async fn get_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send("GET", &dataset_path(instance_id, namespace, name), None)
            .await
    }
}

/// The URI every namespace operation binds: `.../instance/{instanceId}/namespaces/{name}`.
fn namespace_path(instance_id: &str, name: &str) -> String {
    format!("/api/datalake/instance/{instance_id}/namespaces/{name}")
}

/// The URI every dataset operation binds.
fn dataset_path(instance_id: &str, namespace: &str, name: &str) -> String {
    format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}")
}

/// The smallest schema the model admits (`DataLakeDatasetSchema$fields` is `@length`
/// 1..500), so a row is about the namespace delete and nothing else.
fn minimal_schema() -> Value {
    json!({
        "name": "SimSchema",
        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
    })
}

/// The `message` member of a refusal, as a client reads it.
fn message_of(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no `message`: {body}"))
        .to_string()
}

/// The wire error code of a refusal (`__type` on restJson1).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no `__type`: {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Namespace names spanning the modelled `DataLakeNamespaceName` shape — `@length` 1..50,
/// `@pattern ^[a-z0-9_]+$` (source: the `name` member of `DeleteDataLakeNamespaceRequest`)
/// — none of them starting with a prefix the service reserves.
fn namespace_rows() -> Vec<(&'static str, String)> {
    vec![
        ("single character: the modelled 1-character minimum", "n".to_string()),
        ("digits and underscores", "zone_42_raw".to_string()),
        // 50 characters: the shape's upper length bound.
        (
            "name at the modelled 50-character maximum",
            format!("z{}", "_9".repeat(24) + "0"),
        ),
    ]
}

/// RULE: while the namespace holds at least one dataset the delete is refused
/// `ValidationException` (400) naming the namespace, and nothing is removed — the
/// namespace stays readable and so does every dataset under it. Once the datasets are
/// gone the delete succeeds, answers with the pair it removed, and the namespace becomes
/// unaddressable for both a read and a repeat delete.
#[tokio::test]
async fn a_namespace_is_deletable_only_once_its_datasets_are_gone() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("delete lifecycle").await;

    // Rows differing along the namespace name AND along how many datasets block the
    // delete: one, and several.
    let rows: Vec<(String, String, Vec<&str>)> = namespace_rows()
        .into_iter()
        .zip([vec!["lines"], vec!["a", "lines_2026", "orders"], vec!["l"]])
        .map(|((case, namespace), datasets)| (case.to_string(), namespace, datasets))
        .collect();

    for (case, namespace, datasets) in rows {
        emulator.given_namespace(&case, &instance, &namespace).await;
        for dataset in &datasets {
            emulator
                .given_dataset(&case, &instance, &namespace, dataset)
                .await;
        }

        let expected = not_empty_sentence(&namespace);
        let (status, body) = emulator.delete_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a delete of a namespace holding {} dataset(s) expected 400, got \
             {status} with {body}",
            datasets.len()
        );
        assert_eq!(
            error_code(&case, &body),
            "ValidationException",
            "case {case:?}: expected a ValidationException envelope, got {body}"
        );
        let message = message_of(&case, &body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(instance.as_str()),
            "case {case:?}: the sentence must not carry the instanceId, got {message:?}"
        );

        // The refused delete had no effect: the namespace is still readable and so is
        // every dataset under it.
        let (status, body) = emulator.get_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refused delete must leave the namespace readable, got {status} \
             with {body}"
        );
        for dataset in &datasets {
            let (status, body) = emulator.get_dataset(&instance, &namespace, dataset).await;
            assert_eq!(
                status, 200,
                "case {case:?}: dataset {dataset:?} must survive the refused delete, got {status} \
                 with {body}"
            );
        }

        for dataset in &datasets {
            let (status, body) = emulator.delete_dataset(&instance, &namespace, dataset).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteDataLakeDataset expected 200, got {status} with {body}"
            );
        }

        let (status, body) = emulator.delete_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an empty namespace must be deletable, got {status} with {body}"
        );
        assert_eq!(
            (
                body.get("instanceId").and_then(Value::as_str),
                body.get("name").and_then(Value::as_str),
            ),
            (Some(instance.as_str()), Some(namespace.as_str())),
            "case {case:?}: the delete must answer the pair it removed, got {body}"
        );
        assert_eq!(
            emulator.stored_namespaces(&instance),
            0,
            "case {case:?}: the deleted namespace must be gone from the store"
        );

        let gone = not_found_sentence(&namespace);
        let (status, body) = emulator.get_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a deleted namespace is no longer readable, got {status} with {body}"
        );
        assert_eq!(
            error_code(&case, &body),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException envelope, got {body}"
        );
        let message = message_of(&case, &body);
        assert_eq!(
            message, gone,
            "case {case:?}: expected {gone:?} reading a deleted namespace, got {message:?}"
        );

        // `@idempotent` does not make a repeat delete succeed: the name names nothing now.
        let (status, body) = emulator.delete_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a repeat delete expected 404, got {status} with {body}"
        );
        let message = message_of(&case, &body);
        assert_eq!(
            message, gone,
            "case {case:?}: expected {gone:?} on a repeat delete, got {message:?}"
        );

        // The name is free again, which is what "removed" means.
        emulator.given_namespace(&case, &instance, &namespace).await;
        let (status, body) = emulator.delete_namespace(&instance, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the re-created namespace must be deletable, got {status} with {body}"
        );
    }
}

/// RULE: only the datasets UNDER the addressed namespace block its delete — datasets of a
/// sibling namespace, of a pre-defined namespace, or of another instance do not.
#[tokio::test]
async fn only_the_addressed_namespaces_own_datasets_block_its_delete() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("sibling datasets").await;
    let other = emulator.given_instance("other instance").await;

    // Datasets that must NOT block `empty`'s delete: one in a sibling namespace of the
    // same instance, one in each namespace the instance comes with, and one under the
    // SAME name in another instance.
    emulator.given_namespace("sibling", &instance, "sibling").await;
    emulator
        .given_dataset("sibling", &instance, "sibling", "lines")
        .await;
    for namespace in PREDEFINED_NAMESPACES {
        emulator
            .given_dataset("pre-defined", &instance, namespace, "inbound_order")
            .await;
    }
    emulator.given_namespace("other", &other, "empty").await;
    emulator
        .given_dataset("other", &other, "empty", "lines")
        .await;

    emulator.given_namespace("empty", &instance, "empty").await;
    let (status, body) = emulator.delete_namespace(&instance, "empty").await;
    assert_eq!(
        status, 200,
        "case \"empty\": a namespace of its own holds no dataset, so its delete must succeed, got \
         {status} with {body}"
    );

    // Nothing else moved: the sibling, the pre-defined datasets and the other instance's
    // namespace are all untouched.
    let (status, body) = emulator.get_dataset(&instance, "sibling", "lines").await;
    assert_eq!(
        status, 200,
        "case \"sibling\": the sibling namespace's dataset must survive, got {status} with {body}"
    );
    assert_eq!(
        emulator.stored_datasets(&instance),
        1 + PREDEFINED_NAMESPACES.len(),
        "case \"sibling\": the delete must remove no dataset"
    );
    let (status, body) = emulator.get_namespace(&other, "empty").await;
    assert_eq!(
        status, 200,
        "case \"other\": the same name under another instance is a different namespace, got \
         {status} with {body}"
    );

    // And the sibling that still holds its dataset is still refused.
    let expected = not_empty_sentence("sibling");
    let (status, body) = emulator.delete_namespace(&instance, "sibling").await;
    assert_eq!(
        status, 400,
        "case \"sibling\": expected 400 while it holds a dataset, got {status} with {body}"
    );
    let message = message_of("sibling", &body);
    assert_eq!(
        message, expected,
        "case \"sibling\": expected {expected:?}, got {message:?}"
    );
}

/// RULE: a name this instance's data lake holds no namespace under names no namespace at
/// all, so the delete answers `ResourceNotFoundException` (404) with the same sentence a
/// read of it carries — whatever the name's form, and whether another instance holds one
/// under it or not.
#[tokio::test]
async fn deleting_a_namespace_the_data_lake_never_held_is_not_found() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("absent").await;
    let holder = emulator.given_instance("holder").await;

    let mut rows = namespace_rows();
    rows.push(("a name another instance holds", "shared_name".to_string()));
    emulator
        .given_namespace("holder", &holder, "shared_name")
        .await;

    for (case, name) in rows {
        let expected = not_found_sentence(&name);
        let (status, body) = emulator.delete_namespace(&instance, &name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException envelope, got {body}"
        );
        let message = message_of(case, &body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The other instance's namespace is untouched by every one of those refusals.
    let (status, body) = emulator.get_namespace(&holder, "shared_name").await;
    assert_eq!(
        status, 200,
        "case \"holder\": the other instance's namespace must still be readable, got {status} \
         with {body}"
    );
}

/// RULE: a namespace the instance comes with goes only with the instance, so a delete
/// addressed to one is refused `ValidationException` (400) — and it stays usable, which a
/// dataset created in it right afterwards shows.
#[tokio::test]
async fn a_pre_defined_namespace_cannot_be_deleted() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("pre-defined").await;

    for namespace in PREDEFINED_NAMESPACES {
        let expected = predefined_sentence(namespace);
        let (status, body) = emulator.delete_namespace(&instance, namespace).await;
        assert_eq!(
            status, 400,
            "case {namespace:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(namespace, &body),
            "ValidationException",
            "case {namespace:?}: expected a ValidationException envelope, got {body}"
        );
        let message = message_of(namespace, &body);
        assert_eq!(
            message, expected,
            "case {namespace:?}: expected {expected:?}, got {message:?}"
        );

        emulator
            .given_dataset(namespace, &instance, namespace, "inbound_order")
            .await;
    }
}

/// RULE: an instance-scoped namespace delete whose `instanceId` names no instance of this
/// account is refused by the gateway's authorization step — before the namespace is
/// looked up at all — so it answers `AccessDeniedException` (403) and removes nothing.
#[tokio::test]
async fn a_delete_naming_an_unknown_instance_is_refused_by_authorization() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("authorization").await;
    emulator.given_namespace("authorization", &instance, "orders").await;

    // A well-formed `UUID` (the modelled shape) that names no instance of this account.
    let unknown = "11111111-2222-3333-4444-555555555555";
    let (status, body) = emulator.delete_namespace(unknown, "orders").await;
    assert_eq!(
        status, 403,
        "case \"unknown instance\": expected 403, got {status} with {body}"
    );
    assert_eq!(
        error_code("unknown instance", &body),
        "AccessDeniedException",
        "case \"unknown instance\": expected an AccessDeniedException envelope, got {body}"
    );
    let message = message_of("unknown instance", &body);
    assert!(
        message.contains(unknown),
        "case \"unknown instance\": the gateway sentence must name the instanceId, got {message:?}"
    );

    // The namespace of the instance that DOES exist is untouched.
    assert_eq!(
        emulator.stored_namespaces(&instance),
        1,
        "case \"unknown instance\": the refused delete must remove nothing"
    );
    let (status, body) = emulator.get_namespace(&instance, "orders").await;
    assert_eq!(
        status, 200,
        "case \"unknown instance\": the real instance's namespace must still be readable, got \
         {status} with {body}"
    );
}
