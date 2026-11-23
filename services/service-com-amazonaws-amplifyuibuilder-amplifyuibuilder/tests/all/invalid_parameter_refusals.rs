// SPDX-License-Identifier: CC0-1.0

//! One refusal class, one answer: a request this service will not accept because of the
//! value of an input parameter.
//!
//! The rule under test: a request refused because it does not satisfy this service's own
//! input rules is answered with the exception the model declares for an invalid input
//! value — `InvalidParameterException`, "An invalid or out-of-range value was supplied for
//! the input parameter", HTTP 400
//! (`smithy_query(query="shape",shape="InvalidParameterException")`; it is a common error
//! of every operation of this service) — in the wire form this service answers every one
//! of its exceptions in: the status and a `message`, naming no error type. Which LAYER
//! found the bad value does not change the answer: a bound the model declares, checked by
//! the framework's validating walk before any handler runs, and a rule only a handler can
//! check are the same refusal to a caller.
//!
//! The rows span the ways a value can fail a bound the model declares — too long, too
//! short, absent, below a range, above a range, outside a pattern — across the operations
//! and members that carry one, and are checked against the refusals this service's own
//! handlers answer with the same exception. Each bound is also exercised ON its published
//! limit, so a row's refusal is about the bound and not about the member.

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

/// The published bounds of the name shapes of this service's named resources:
/// `ComponentName`, `FormName` and `ThemeName` are each `@length(min: 1, max: 255)`
/// (`smithy_query(query="shape",shape="ComponentName,FormName,ThemeName")`).
const NAME_MIN_LENGTH: usize = 1;
const NAME_MAX_LENGTH: usize = 255;

/// The published bounds of `ListEntityLimit`, the shape `maxResults` targets:
/// `@range(min: 1, max: 100)` (`smithy_query(query="shape",shape="ListEntityLimit")`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 100;

/// The published maximum length of `AppId`, the shape `StartCodegenJob$appId` targets, and
/// the expression it publishes.
const APP_ID_MAX_LENGTH: usize = 20;
const APP_ID_PATTERN: &str = "^d[a-z0-9]+$";

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status, the JSON body, and the error-type header.
struct Answer {
    status: u16,
    body: Value,
    error_type_header: Option<String>,
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

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "amplifyuibuilder.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_AMPLIFYUIBUILDER);
    if !payload.is_empty() {
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
    Answer {
        status,
        body,
        error_type_header,
    }
}

/// One request that fails a rule this service's MODEL declares, with the member the rule
/// sits on and the requirement the value failed.
struct BoundCase {
    /// What the row varies, for the assertion messages.
    label: &'static str,
    method: &'static str,
    uri: String,
    payload: Option<Value>,
    /// The input member path the refused value sat at, as a refusal states it: the trail of
    /// model member names from the operation's input inwards, lowerCamel.
    path: &'static str,
    /// The requirement the value failed, as a refusal states it.
    requirement: String,
}

/// The length requirements a refusal states, over the bound the model publishes.
fn at_least_long(min: usize) -> String {
    format!("Member must have length greater than or equal to {min}")
}

fn at_most_long(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The range requirements, over the bound the model publishes.
fn at_least(min: i64) -> String {
    format!("Member must have value greater than or equal to {min}")
}

fn at_most(max: i64) -> String {
    format!("Member must have value less than or equal to {max}")
}

/// The create payloads carrying only the members each create data shape requires:
/// `CreateComponentData`'s six, `CreateFormData`'s seven, `CreateThemeData`'s one plus
/// `values`, and `StartCodegenJobData`'s one.
fn component_data(name: Value, component_type: Value) -> Value {
    json!({
        "name": name,
        "componentType": component_type,
        "properties": {},
        "variants": [],
        "overrides": {},
        "bindingProperties": {},
    })
}

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

fn theme_data(name: &str) -> Value {
    json!({"name": name, "values": []})
}

fn codegen_job_data() -> Value {
    json!({"renderConfig": {"react": {}}})
}

fn collection_uri(app_id: &str, environment: &str, collection: &str) -> String {
    format!("/app/{app_id}/environment/{environment}/{collection}")
}

/// An entity name of exactly `length` characters that this service ACCEPTS: `letter`
/// repeated, with the initial upper-cased.
///
/// A name whose initial is not an upper-case letter is refused by the handler whatever
/// its length (`an_entity_name_the_service_does_not_accept_is_refused` below holds that
/// rule), so the rows that exercise a published LENGTH bound on its own build their names
/// here — the bound is then the only thing such a row is about.
fn upper_initial_name(letter: &str, length: usize) -> String {
    let name = letter.repeat(length);
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => name,
    }
}

/// Every way an input value can fail a rule the model declares, over the operations and
/// members that declare one.
fn bound_cases() -> Vec<BoundCase> {
    let components = collection_uri(APP_ID, ENVIRONMENT, "components");
    vec![
        BoundCase {
            label: "a component name one longer than @length max",
            method: "POST",
            uri: components.clone(),
            payload: Some(component_data(
                json!("n".repeat(NAME_MAX_LENGTH + 1)),
                json!("View"),
            )),
            path: "componentToCreate.name",
            requirement: at_most_long(NAME_MAX_LENGTH),
        },
        BoundCase {
            label: "a component name one shorter than @length min",
            method: "POST",
            uri: components.clone(),
            payload: Some(component_data(
                json!("n".repeat(NAME_MIN_LENGTH - 1)),
                json!("View"),
            )),
            path: "componentToCreate.name",
            requirement: at_least_long(NAME_MIN_LENGTH),
        },
        BoundCase {
            label: "a component type one longer than @length max",
            method: "POST",
            uri: components.clone(),
            payload: Some(component_data(
                json!("Typed"),
                json!("T".repeat(NAME_MAX_LENGTH + 1)),
            )),
            path: "componentToCreate.componentType",
            requirement: at_most_long(NAME_MAX_LENGTH),
        },
        BoundCase {
            label: "a component create missing the required name",
            method: "POST",
            uri: components.clone(),
            payload: Some(json!({
                "componentType": "View",
                "properties": {},
                "variants": [],
                "overrides": {},
                "bindingProperties": {},
            })),
            path: "componentToCreate.name",
            requirement: "Member must not be null".to_string(),
        },
        BoundCase {
            label: "a form name one longer than @length max",
            method: "POST",
            uri: collection_uri(APP_ID, ENVIRONMENT, "forms"),
            payload: Some(form_data(&"f".repeat(NAME_MAX_LENGTH + 1))),
            path: "formToCreate.name",
            requirement: at_most_long(NAME_MAX_LENGTH),
        },
        BoundCase {
            label: "a theme name one shorter than @length min",
            method: "POST",
            uri: collection_uri(APP_ID, ENVIRONMENT, "themes"),
            payload: Some(theme_data(&"t".repeat(NAME_MIN_LENGTH - 1))),
            path: "themeToCreate.name",
            requirement: at_least_long(NAME_MIN_LENGTH),
        },
        BoundCase {
            label: "a maxResults one below @range min",
            method: "GET",
            uri: format!("{components}?maxResults={}", MAX_RESULTS_MIN - 1),
            payload: None,
            path: "maxResults",
            requirement: at_least(MAX_RESULTS_MIN),
        },
        BoundCase {
            label: "a maxResults one above @range max",
            method: "GET",
            uri: format!("{components}?maxResults={}", MAX_RESULTS_MAX + 1),
            payload: None,
            path: "maxResults",
            requirement: at_most(MAX_RESULTS_MAX),
        },
        BoundCase {
            label: "an appId outside the @pattern AppId declares",
            method: "POST",
            uri: collection_uri("NotAnAppId", ENVIRONMENT, "codegen-jobs"),
            payload: Some(codegen_job_data()),
            path: "appId",
            requirement: format!(
                "Member must satisfy regular expression pattern: {APP_ID_PATTERN}"
            ),
        },
        BoundCase {
            label: "an appId one longer than the @length max AppId declares",
            method: "POST",
            uri: collection_uri(
                &format!("d{}", "a".repeat(APP_ID_MAX_LENGTH)),
                ENVIRONMENT,
                "codegen-jobs",
            ),
            payload: Some(codegen_job_data()),
            path: "appId",
            requirement: at_most_long(APP_ID_MAX_LENGTH),
        },
    ]
}

/// Every row is refused with the one exception this service declares for an invalid input
/// value: HTTP 400, no error type anywhere a client reads one, and the message that states
/// the member path and the requirement the value failed.
#[tokio::test]
async fn a_value_outside_a_modelled_bound_is_an_invalid_parameter() {
    let router = emulator();

    for case in bound_cases() {
        let answer = call(&router, case.method, &case.uri, case.payload).await;
        let label = case.label;
        assert_eq!(
            answer.status, 400,
            "case {label:?}: an invalid input value must be refused with HTTP 400, got {} \
             with {:?}",
            answer.status, answer.body
        );
        assert!(
            !answer.names_an_error_type(),
            "case {label:?}: the refusal must name no error type, got header {:?} and body {:?}",
            answer.error_type_header,
            answer.body
        );
        let message = answer.message();
        assert!(
            !message.is_empty(),
            "case {label:?}: the refusal must still say why, got {:?}",
            answer.body
        );
        // One refused value is one violation, stated in the envelope AWS words a failed
        // input requirement in: the count, the value that was refused, the member path it
        // sat at, and the requirement it failed.
        let clause = format!(
            "at '{}' failed to satisfy constraint: {}",
            case.path, case.requirement
        );
        assert!(
            message.starts_with("1 validation error detected: ") && message.ends_with(&clause),
            "case {label:?}: the message must state one violation ending {clause:?}, got \
             {message:?}"
        );
        assert!(
            answer.id().is_empty(),
            "case {label:?}: a refused call must mint nothing, got {:?}",
            answer.body
        );
    }

    // Nothing in the table was stored: every refusal was answered before any handler ran.
    let listed = call(
        &router,
        "GET",
        &collection_uri(APP_ID, ENVIRONMENT, "components"),
        None,
    )
    .await;
    assert_eq!(
        listed.body["entities"],
        json!([]),
        "a refused request must store nothing, got {:?}",
        listed.body
    );
}

/// The published limit of each bound is ACCEPTED, so each row above is refused for its
/// value and not for the member it sat at.
#[tokio::test]
async fn each_published_bound_is_itself_accepted() {
    let router = emulator();

    for (label, method, uri, payload) in [
        (
            "a component name at @length min",
            "POST",
            collection_uri(APP_ID, ENVIRONMENT, "components"),
            Some(component_data(
                json!(upper_initial_name("n", NAME_MIN_LENGTH)),
                json!("View"),
            )),
        ),
        (
            "a component name at @length max",
            "POST",
            collection_uri(APP_ID, ENVIRONMENT, "components"),
            Some(component_data(
                json!(upper_initial_name("m", NAME_MAX_LENGTH)),
                json!("View"),
            )),
        ),
        (
            "a component type at @length max",
            "POST",
            collection_uri(APP_ID, ENVIRONMENT, "components"),
            Some(component_data(
                json!("Typed"),
                json!("T".repeat(NAME_MAX_LENGTH)),
            )),
        ),
        (
            "a form name at @length max",
            "POST",
            collection_uri(APP_ID, ENVIRONMENT, "forms"),
            Some(form_data(&upper_initial_name("f", NAME_MAX_LENGTH))),
        ),
        (
            "a theme name at @length min",
            "POST",
            collection_uri(APP_ID, ENVIRONMENT, "themes"),
            Some(theme_data(&upper_initial_name("t", NAME_MIN_LENGTH))),
        ),
        (
            "a maxResults at @range min",
            "GET",
            format!(
                "{}?maxResults={MAX_RESULTS_MIN}",
                collection_uri(APP_ID, ENVIRONMENT, "components")
            ),
            None,
        ),
        (
            "a maxResults at @range max",
            "GET",
            format!(
                "{}?maxResults={MAX_RESULTS_MAX}",
                collection_uri(APP_ID, ENVIRONMENT, "components")
            ),
            None,
        ),
        (
            "an appId at the @length max AppId declares, matching its @pattern",
            "POST",
            collection_uri(
                &format!("d{}", "a".repeat(APP_ID_MAX_LENGTH - 1)),
                ENVIRONMENT,
                "codegen-jobs",
            ),
            Some(codegen_job_data()),
        ),
    ] {
        let answer = call(&router, method, &uri, payload).await;
        assert_eq!(
            answer.status, 200,
            "case {label:?}: a value on its published bound must be accepted, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

/// The same class found by a HANDLER is answered in the same form, so a client cannot tell
/// the two layers apart: both are HTTP 400 with a message and no error type.
#[tokio::test]
async fn a_handler_found_invalid_value_is_answered_in_the_same_form() {
    let router = emulator();
    let components = collection_uri(APP_ID, ENVIRONMENT, "components");

    let handler_found = vec![
        (
            "a reserved property name on CreateComponent",
            call(
                &router,
                "POST",
                &components,
                Some(json!({
                    "name": "Reserved",
                    "componentType": "View",
                    "properties": {"tags": {"value": "x"}},
                    "variants": [],
                    "overrides": {},
                    "bindingProperties": {},
                })),
            )
            .await,
        ),
        (
            "a nextToken ListComponents never issued",
            call(
                &router,
                "GET",
                &format!("{components}?nextToken=bogus"),
                None,
            )
            .await,
        ),
        (
            "an unknown metadata feature on PutMetadataFlag",
            call(
                &router,
                "PUT",
                &format!("/app/{APP_ID}/environment/{ENVIRONMENT}/metadata/features/nope"),
                Some(json!({"newValue": "true"})),
            )
            .await,
        ),
    ];

    // The framework-found refusal every row is compared against.
    let boundary_found = call(
        &router,
        "POST",
        &components,
        Some(component_data(
            json!("n".repeat(NAME_MAX_LENGTH + 1)),
            json!("View"),
        )),
    )
    .await;

    for (label, answer) in handler_found {
        assert_eq!(
            answer.status, boundary_found.status,
            "case {label:?}: a handler-found invalid value must answer the status a \
             boundary-found one answers ({}), got {} with {:?}",
            boundary_found.status, answer.status, answer.body
        );
        assert_eq!(
            answer.names_an_error_type(),
            boundary_found.names_an_error_type(),
            "case {label:?}: both layers must name an error type alike, got header {:?} and \
             body {:?}",
            answer.error_type_header,
            answer.body
        );
        assert!(
            !answer.message().is_empty(),
            "case {label:?}: the refusal must say why, got {:?}",
            answer.body
        );
    }
}

/// The message this service answers for an entity name it will not create or rename an
/// entity under: one fixed sentence, naming neither the name it refused nor the kind of
/// entity it was refused for.
const ENTITY_NAME_NOT_VALID: &str = "Entity name is not a valid name";

/// One entity kind of this service, with the payloads a create and a rename of one take.
///
/// The three kinds — a component, a form and a theme — are the entities the name rule is
/// about, so each row of the table below is driven against all three.
struct EntityKind {
    collection: &'static str,
    /// The create payload carrying only the members that kind's create data requires,
    /// under the name the row supplies.
    create: fn(&str) -> Value,
    /// The rename payload: the name, plus whatever else that kind's update data requires
    /// (`UpdateThemeData$values` is `@required`, the other two require nothing).
    rename: fn(&str) -> Value,
}

fn entity_kinds() -> Vec<EntityKind> {
    vec![
        EntityKind {
            collection: "components",
            create: |name| component_data(json!(name), json!("View")),
            rename: |name| json!({"name": name}),
        },
        EntityKind {
            collection: "forms",
            create: form_data,
            rename: |name| json!({"name": name}),
        },
        EntityKind {
            collection: "themes",
            create: theme_data,
            rename: |name| json!({"name": name, "values": []}),
        },
    ]
}

/// The names this service does NOT accept for an entity, differing along the one thing the
/// rule is about — the name's INITIAL character — with the rest of the name left well
/// formed: the recorded case is the first row, which is the accepted name of the table
/// below with its initial lower-cased.
fn names_the_service_refuses() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a lower-case initial", "postCreateForm"),
        ("a lower-case initial on a one-character name", "f"),
        ("a digit initial", "2024Form"),
        ("an underscore initial", "_Form"),
        ("a hyphen initial", "-Form"),
        ("a space initial", " Form"),
        ("a lower-case non-ASCII initial", "überschrift"),
    ]
}

/// The names it accepts, differing along the same thing plus what follows the initial, so
/// a row above is refused for its initial and not for the rest of the name or its length.
fn names_the_service_accepts() -> Vec<(&'static str, String)> {
    vec![
        ("an upper-case initial", "PostCreateForm".to_string()),
        ("a one-character upper-case name", "F".to_string()),
        ("an upper-case non-ASCII initial", "Überschrift".to_string()),
        ("punctuation and digits past the initial", "Form_v2-1".to_string()),
        (
            "an upper-case initial at the @length max",
            upper_initial_name("n", NAME_MAX_LENGTH),
        ),
    ]
}

/// An entity name this service does not accept is refused with the same
/// `InvalidParameterException` every other invalid input value is answered with — HTTP
/// 400, no error type, and the one fixed sentence — on EVERY write path that sets an
/// entity's name: the create and the rename of a component, a form and a theme alike. A
/// refused create stores nothing and a refused rename leaves the stored name alone.
#[tokio::test]
async fn an_entity_name_the_service_does_not_accept_is_refused() {
    for kind in entity_kinds() {
        let router = emulator();
        let collection = collection_uri(APP_ID, ENVIRONMENT, kind.collection);

        // The entity a rename is attempted on, under a name the service accepts.
        let live = call(&router, "POST", &collection, Some((kind.create)("Live"))).await;
        assert_eq!(
            live.status, 200,
            "case {}: the fixture create must succeed, got {} with {:?}",
            kind.collection, live.status, live.body
        );
        let live_id = live.id();

        for (form, name) in names_the_service_refuses() {
            let case = format!("{} / {form}", kind.collection);

            for (operation, answer) in [
                (
                    "the create",
                    call(&router, "POST", &collection, Some((kind.create)(name))).await,
                ),
                (
                    "the rename",
                    call(
                        &router,
                        "PATCH",
                        &format!("{collection}/{live_id}"),
                        Some((kind.rename)(name)),
                    )
                    .await,
                ),
            ] {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: {operation} naming {name:?} must be refused with HTTP \
                     400, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.message(),
                    ENTITY_NAME_NOT_VALID,
                    "case {case:?}: {operation} must answer the message \
                     {ENTITY_NAME_NOT_VALID:?}, got {:?}",
                    answer.message()
                );
                assert!(
                    !answer.names_an_error_type(),
                    "case {case:?}: {operation} must name no error type, got header {:?} and \
                     body {:?}",
                    answer.error_type_header,
                    answer.body
                );
            }

            // The refused create minted nothing and the refused rename moved nothing: the
            // fixture is still the one entity of this app environment, under its own name.
            let listed = call(&router, "GET", &collection, None).await;
            let entities = listed.body["entities"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                entities.len(),
                1,
                "case {case:?}: a refused write must leave the one fixture entity, got {:?}",
                listed.body
            );
            let read = call(&router, "GET", &format!("{collection}/{live_id}"), None).await;
            assert_eq!(
                read.body["name"],
                json!("Live"),
                "case {case:?}: a refused rename must leave the stored name alone, got {:?}",
                read.body
            );
        }
    }
}

/// The names the service DOES accept are created and renamed to, so each row above is
/// refused for the initial the rule is about rather than for the member it sat at.
#[tokio::test]
async fn an_entity_name_the_service_accepts_is_created_and_renamed_to() {
    for kind in entity_kinds() {
        let collection = collection_uri(APP_ID, ENVIRONMENT, kind.collection);

        for (form, name) in names_the_service_accepts() {
            let case = format!("{} / {form}", kind.collection);
            let router = emulator();

            let created = call(&router, "POST", &collection, Some((kind.create)(&name))).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: creating {name:?} must succeed, got {} with {:?}",
                created.status, created.body
            );
            assert_eq!(
                created.body["name"],
                json!(name),
                "case {case:?}: the created entity must carry the name it was created \
                 with, got {:?}",
                created.body
            );

            // And the same name is one a rename may move another entity onto — in another
            // app environment, since the entity just created holds it in this one.
            let elsewhere = collection_uri("d9999zzzz", ENVIRONMENT, kind.collection);
            let mover = call(&router, "POST", &elsewhere, Some((kind.create)("Mover"))).await;
            let renamed = call(
                &router,
                "PATCH",
                &format!("{elsewhere}/{}", mover.id()),
                Some((kind.rename)(&name)),
            )
            .await;
            assert_eq!(
                renamed.status, 200,
                "case {case:?}: renaming onto {name:?} must succeed, got {} with {:?}",
                renamed.status, renamed.body
            );
            let read = call(&router, "GET", &format!("{elsewhere}/{}", mover.id()), None).await;
            assert_eq!(
                read.body["name"],
                json!(name),
                "case {case:?}: the renamed entity must read back under its new name, got \
                 {:?}",
                read.body
            );
        }
    }
}
