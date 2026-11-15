// SPDX-License-Identifier: CC0-1.0

//! One refusal class, one answer: a request this service could not READ.
//!
//! The rule under test: a request AmplifyUIBuilder refuses is answered in the one wire form
//! this service answers every refusal in — the status and a `message`, naming no error type
//! in either place a client reads one (the `x-amzn-errortype` header restJson1 resolves an
//! error shape from, and a `__type` / `code` / `Code` member in the body) — and that holds
//! for a request whose BODY the service could not read at all just as it does for one whose
//! values it read and then refused. Which layer found the fault does not change the answer:
//! the codec, the framework's validating walk, and a handler are all this service refusing a
//! call.
//!
//! A body the codec cannot read reaches no refusal seam — the framework has no structured
//! description for it, so it words that refusal itself, with a shape
//! `com.amazonaws.amplifyuibuilder` declares nowhere — which is why the rows below assert on
//! what a CLIENT reads rather than on a code: a refusal of this service names nothing.
//!
//! The rows span the ways a body can fail to be read — not well-formed JSON (truncated, an
//! illegal literal, a trailing comma, text that is not JSON at all), a JSON type other than
//! the modelled one (a number, a boolean, an array, an object or a string where a string, a
//! boolean, a map, a list or a structure is modelled), at the top of the payload and nested
//! inside a map entry — across the operations that bind an `@httpPayload` body
//! (`smithy_query(query="operation",operation="CreateForm,CreateComponent,CreateTheme,UpdateForm,StartCodegenJob,PutMetadataFlag")`),
//! and are checked against the refusals the framework's bound walk and this service's own
//! handlers answer.
//!
//! Each suite in this crate carries its own request harness, so this one does too.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service: it is what routes a request here.
const SIGNED_FOR_AMPLIFYUIBUILDER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/amplifyuibuilder/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The app and backend environment every case addresses. `AppId` is `@length(min: 1,
/// max: 20)` with pattern `^d[a-z0-9]+$` (`smithy_query(query="shape",shape="AppId")`).
const APP_ID: &str = "d1234abcd";
const ENVIRONMENT: &str = "staging";

/// The response header restJson1 resolves an error shape from. It must be absent from
/// every refusal this service answers.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The published maximum length of the name shapes of this service's named resources:
/// `FormName` is `@length(min: 1, max: 255)`
/// (`smithy_query(query="shape",shape="FormName")`). Used for the bound-violation refusal
/// the unreadable-body rows are compared against.
const NAME_MAX_LENGTH: usize = 255;

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status, the body (JSON when it parses) and the
/// error-type header.
struct Answer {
    status: u16,
    body: Value,
    /// The body exactly as it arrived, for an assertion message when it is not JSON.
    raw: String,
    error_type_header: Option<String>,
}

impl Answer {
    /// The resource id the answer carries, or the empty string. Every create and update of
    /// this service binds its entity to `@httpPayload`, so the body IS the resource.
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

    /// The error-type discriminator the body carries — `__type`, or the `code` / `Code`
    /// spellings the same JSON error parsers also accept — or the empty string.
    fn body_error_code(&self) -> &str {
        ["__type", "code", "Code"]
            .iter()
            .find_map(|member| self.body[*member].as_str())
            .unwrap_or_default()
    }

    /// Whether the answer names an error type anywhere a client would read one.
    fn names_an_error_type(&self) -> bool {
        self.error_type_header.is_some() || !self.body_error_code().is_empty()
    }
}

/// One request, with its body written as the client sent it: raw text, because a body that
/// is not well-formed JSON cannot be built from a JSON value.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> Answer {
    let payload = body.unwrap_or_default().to_string();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "amplifyuibuilder.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_AMPLIFYUIBUILDER);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type_header = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer {
        status,
        body,
        raw,
        error_type_header,
    }
}

/// A request carrying a JSON value, for the cases whose body IS well-formed.
async fn call_json(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Value,
) -> Answer {
    call(router, method, uri, Some(&body.to_string())).await
}

fn collection_uri(collection: &str) -> String {
    format!("/app/{APP_ID}/environment/{ENVIRONMENT}/{collection}")
}

/// The create payload carrying only the members `CreateFormData` requires.
fn form_data(name: &str) -> Value {
    json!({
        "name": name,
        "formActionType": "create",
        "dataType": {"dataSourceType": "DataStore", "dataTypeName": "Post"},
        "fields": {},
        "style": {},
        "sectionalElements": {},
        "schemaVersion": "1.0",
    })
}

/// The create payload carrying only the members `CreateComponentData` requires.
fn component_data(name: &str) -> Value {
    json!({
        "name": name,
        "componentType": "View",
        "properties": {},
        "variants": [],
        "overrides": {},
        "bindingProperties": {},
    })
}

/// One request whose body this service cannot read.
struct UnreadableCase {
    /// What the row varies, for the assertion messages.
    label: &'static str,
    method: &'static str,
    uri: String,
    body: &'static str,
}

/// Every way a body can fail to be read, over the operations that bind an `@httpPayload`
/// body: the two halves are a body that is not well-formed JSON, and a body that is
/// well-formed but carries a JSON type other than the one its member models.
fn unreadable_cases(form_id: &str) -> Vec<UnreadableCase> {
    let forms = collection_uri("forms");
    let components = collection_uri("components");
    let themes = collection_uri("themes");
    let codegen_jobs = collection_uri("codegen-jobs");
    let one_form = format!("{forms}/{form_id}");
    let metadata_flag = format!("/app/{APP_ID}/environment/{ENVIRONMENT}/metadata/features/nope");

    vec![
        // Not well-formed JSON.
        UnreadableCase {
            label: "a body truncated after a member name, on CreateForm",
            method: "POST",
            uri: forms.clone(),
            body: r#"{"name":"#,
        },
        UnreadableCase {
            label: "a body that is a bare opening brace, on UpdateForm",
            method: "PATCH",
            uri: one_form.clone(),
            body: "{",
        },
        UnreadableCase {
            label: "a trailing comma, on CreateComponent",
            method: "POST",
            uri: components.clone(),
            body: r#"{"name":"Hero",}"#,
        },
        UnreadableCase {
            label: "a body that is not JSON at all, on CreateTheme",
            method: "POST",
            uri: themes.clone(),
            body: "not json at all",
        },
        UnreadableCase {
            label: "an unclosed object, on StartCodegenJob",
            method: "POST",
            uri: codegen_jobs.clone(),
            body: r#"{"renderConfig":{"react":{}}"#,
        },
        UnreadableCase {
            label: "an illegal literal, on PutMetadataFlag",
            method: "PUT",
            uri: metadata_flag.clone(),
            body: r#"{"newValue":tru}"#,
        },
        // Well-formed JSON carrying a type the member does not model.
        UnreadableCase {
            label: "a number where CreateFormData$name models a string",
            method: "POST",
            uri: forms.clone(),
            body: r#"{"name":5,"formActionType":"create","dataType":{"dataSourceType":"DataStore","dataTypeName":"Post"},"fields":{},"style":{},"sectionalElements":{},"schemaVersion":"1.0"}"#,
        },
        UnreadableCase {
            label: "an array where CreateFormData$fields models a map",
            method: "POST",
            uri: forms.clone(),
            body: r#"{"name":"Typed","formActionType":"create","dataType":{"dataSourceType":"DataStore","dataTypeName":"Post"},"fields":[],"style":{},"sectionalElements":{},"schemaVersion":"1.0"}"#,
        },
        UnreadableCase {
            label: "a string where CreateFormData$dataType models a structure",
            method: "POST",
            uri: forms.clone(),
            body: r#"{"name":"Typed","formActionType":"create","dataType":"DataStore","fields":{},"style":{},"sectionalElements":{},"schemaVersion":"1.0"}"#,
        },
        UnreadableCase {
            label: "an array where the CreateForm payload models a structure",
            method: "POST",
            uri: forms.clone(),
            body: "[]",
        },
        UnreadableCase {
            label: "an object where CreateComponentData$variants models a list",
            method: "POST",
            uri: components.clone(),
            body: r#"{"name":"Typed","componentType":"View","properties":{},"variants":{},"overrides":{},"bindingProperties":{}}"#,
        },
        UnreadableCase {
            label: "a number in a CreateComponentData$tags entry, which models a string",
            method: "POST",
            uri: components.clone(),
            body: r#"{"name":"Tagged","componentType":"View","properties":{},"variants":[],"overrides":{},"bindingProperties":{},"tags":{"team":5}}"#,
        },
        UnreadableCase {
            label: "an object where CreateThemeData$values models a list",
            method: "POST",
            uri: themes.clone(),
            body: r#"{"name":"Typed","values":{}}"#,
        },
        UnreadableCase {
            label: "a string where StartCodegenJobData$autoGenerateForms models a boolean",
            method: "POST",
            uri: codegen_jobs.clone(),
            body: r#"{"renderConfig":{"react":{}},"autoGenerateForms":"yes"}"#,
        },
        UnreadableCase {
            label: "a boolean where PutMetadataFlagBody$newValue models a string",
            method: "PUT",
            uri: metadata_flag.clone(),
            body: r#"{"newValue":true}"#,
        },
        UnreadableCase {
            label: "a number where UpdateFormData$schemaVersion models a string",
            method: "PATCH",
            uri: one_form.clone(),
            body: r#"{"schemaVersion":2}"#,
        },
    ]
}

/// Every row is refused the way this service refuses a call: HTTP 400, a `message` that
/// says why, no error type anywhere a client reads one, and nothing minted.
#[tokio::test]
async fn an_unreadable_body_is_refused_naming_no_error_type() {
    let router = emulator();
    // One real form, so an UpdateForm row is refused for its BODY and not for its id.
    let form_id = call_json(&router, "POST", &collection_uri("forms"), form_data("Owned"))
        .await
        .id();
    assert!(
        !form_id.is_empty(),
        "the fixture form must be created before the rows that address it"
    );

    for case in unreadable_cases(&form_id) {
        let answer = call(&router, case.method, &case.uri, Some(case.body)).await;
        let label = case.label;
        assert_eq!(
            answer.status, 400,
            "case {label:?}: a body this service cannot read must be refused with HTTP 400, \
             got {} with {:?}",
            answer.status, answer.raw
        );
        assert!(
            !answer.names_an_error_type(),
            "case {label:?}: the refusal must name no error type, got header {:?} and body {:?}",
            answer.error_type_header,
            answer.raw
        );
        assert!(
            !answer.message().is_empty(),
            "case {label:?}: the refusal must still say why, got {:?}",
            answer.raw
        );
        assert!(
            answer.id().is_empty(),
            "case {label:?}: a refused call must mint nothing, got {:?}",
            answer.raw
        );
    }
}

/// The refusals of a request this service could not read, one whose values it read and then
/// found outside a modelled bound, and one a handler refused are indistinguishable in form:
/// same status, and none of them names an error type.
#[tokio::test]
async fn an_unreadable_body_is_answered_as_every_other_refusal_is() {
    let router = emulator();
    let forms = collection_uri("forms");
    let components = collection_uri("components");

    let unreadable = call(&router, "POST", &forms, Some(r#"{"name":"#)).await;
    let wrong_json_type = call(
        &router,
        "POST",
        &forms,
        Some(r#"{"name":5,"formActionType":"create","dataType":{"dataSourceType":"DataStore","dataTypeName":"Post"},"fields":{},"style":{},"sectionalElements":{},"schemaVersion":"1.0"}"#),
    )
    .await;
    // The framework's bound walk: a name one longer than the `@length` max the model
    // publishes for it.
    let outside_a_bound = call_json(
        &router,
        "POST",
        &forms,
        form_data(&"f".repeat(NAME_MAX_LENGTH + 1)),
    )
    .await;
    // A handler's own rule: a component property named after a reserved word.
    let mut reserved = component_data("Reserved");
    reserved["properties"] = json!({"tags": {"value": "x"}});
    let handler_found = call_json(&router, "POST", &components, reserved).await;

    for (label, answer) in [
        ("a body that is not well-formed JSON", &unreadable),
        ("a member carrying the wrong JSON type", &wrong_json_type),
        ("a value outside a modelled bound", &outside_a_bound),
        ("a rule only a handler can check", &handler_found),
    ] {
        // The modelled status first, so this comparison cannot pass by every layer
        // regressing to the same wrong one, and then the agreement between the layers.
        assert_eq!(
            answer.status, 400,
            "case {label:?}: this refusal class is HTTP 400, got {} with {:?}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.status, outside_a_bound.status,
            "case {label:?}: every refusal of this class must answer the one status ({}), got \
             {} with {:?}",
            outside_a_bound.status, answer.status, answer.raw
        );
        assert!(
            !answer.names_an_error_type(),
            "case {label:?}: the refusal must name no error type, got header {:?} and body {:?}",
            answer.error_type_header,
            answer.raw
        );
        assert!(
            !answer.message().is_empty(),
            "case {label:?}: the refusal must still say why, got {:?}",
            answer.raw
        );
    }
}

/// A refused request changes nothing: the one stored form is still readable, unchanged, and
/// no collection gained a resource from a body that was never read.
#[tokio::test]
async fn a_request_that_could_not_be_read_stores_nothing() {
    let router = emulator();
    let forms = collection_uri("forms");
    let created = call_json(&router, "POST", &forms, form_data("Owned")).await;
    let form_id = created.id();
    assert_eq!(
        created.status, 200,
        "the fixture form must be created, got {} with {:?}",
        created.status, created.raw
    );

    for case in unreadable_cases(&form_id) {
        let answer = call(&router, case.method, &case.uri, Some(case.body)).await;
        assert_eq!(
            answer.status, 400,
            "case {:?}: expected HTTP 400, got {} with {:?}",
            case.label, answer.status, answer.raw
        );
    }

    // Read back through the implemented reader: the create's own record, untouched by the
    // rows that addressed it.
    let read = call(&router, "GET", &format!("{forms}/{form_id}"), None).await;
    assert_eq!(
        read.status, 200,
        "the form must still be readable, got {} with {:?}",
        read.status, read.raw
    );
    assert_eq!(
        read.body["name"],
        json!("Owned"),
        "a refused call must not change the stored form, got {:?}",
        read.body
    );
    assert_eq!(
        read.body["schemaVersion"],
        json!("1.0"),
        "a refused update must not change a member it could not be read for, got {:?}",
        read.body
    );

    for (collection, expected) in [("forms", 1usize), ("components", 0), ("themes", 0)] {
        let listed = call(&router, "GET", &collection_uri(collection), None).await;
        let entities = listed.body["entities"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            entities, expected,
            "case {collection:?}: a refused call must add nothing, expected {expected} \
             entities, got {:?}",
            listed.raw
        );
    }
}
