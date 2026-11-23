// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REQUEST VALIDATORS and MODELS an imported API definition
//! materialises, read back through `GetRequestValidators`, `GetModels`, `GetModel` and
//! `GetMethod`, and then enforced at a deployed stage.
//!
//! THE RULE UNDER TEST: an import carries over every API Gateway vendor extension the
//! definition declares that a record of the api exists for. So each entry of
//! `x-amazon-apigateway-request-validators` becomes a `RequestValidator` named by that entry's
//! key and carrying the `validateRequestBody`/`validateRequestParameters` it declares; the
//! `x-amazon-apigateway-request-validator` the document selects is the validator every
//! operation is imported under and the one an operation selects for itself overrides it, which
//! is what `GetMethod` reports as `requestValidatorId`; and every schema the document NAMES
//! (Swagger 2.0's `definitions`, OpenAPI 3's `components.schemas`) becomes a `Model` under that
//! name, which an operation's declared body references as its `requestModels` entry for that
//! media type. A deployed stage then applies that validator to a call before the integration
//! runs: a body the model does not admit is refused `400`
//! `{"message":"Invalid request body"}`, while a method whose validator checks parameters only
//! serves the identical body.
//!
//! The rows span what the rule generalises over: the spelling the document is written in
//! (Swagger 2.0 and OpenAPI 3, which name their schemas and their bodies differently), the
//! flag combinations a declared entry carries, which selector is in force (the document's, an
//! operation's override, and one naming no declared entry), and whether the posted body
//! satisfies the referenced model.
//!
//! Every case drives the real wire path — the import through `restJson1`, the enforcement on
//! the api's own invoke host — so what is asserted is what a client sees.

use serde_json::{json, Value};

use crate::execute_api_data_plane::deploy;
use crate::rest_api_create_from_import::import;
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// The Region the invoke host of a deployed import is addressed in.
const REGION: &str = "us-east-1";

/// The stage the imported api is deployed to.
const STAGE: &str = "s";

/// The validator entry that checks the BODY alone — the one the document selects for every
/// operation that overrides it with none.
const BODY_ONLY: &str = "parcelbody";

/// The validator entry that checks the PARAMETERS alone, which one operation selects for
/// itself.
const PARAMETERS_ONLY: &str = "parcelparams";

/// The validator entry that checks BOTH halves, declared and selected by nothing: a declared
/// entry becomes a record whether or not an operation names it.
const BOTH: &str = "parcelboth";

/// A selector naming no declared entry, which names no validator of the api.
const UNDECLARED: &str = "nosuchvalidator";

/// The model the operations' bodies reference, which the document names.
const MODEL: &str = "Parcel";

/// The sibling model that one references, which the stored schema names by the api's own
/// model-schema URL.
const NESTED_MODEL: &str = "Note";

/// The body the imported `MOCK` integration answers, which says the integration RAN.
const SERVED: &str = r#"{"served":true}"#;

/// The body API Gateway's `BAD_REQUEST_BODY` gateway response carries, which is what a call
/// the method's request validator refuses is answered with.
const INVALID_REQUEST_BODY: &str = "Invalid request body";

/// The `x-amazon-apigateway-integration` every operation of these definitions declares: a
/// `MOCK` whose rendered request names `200` and whose integration response answers
/// [`SERVED`], so a call that got past the validator is visibly served.
fn mock_integration() -> String {
    format!(
        "{{\"type\": \"mock\", \
           \"requestTemplates\": {{\"application/json\": \"{{\\\"statusCode\\\": 200}}\"}}, \
           \"responses\": {{\"default\": {{\"statusCode\": \"200\", \
             \"responseTemplates\": {{\"application/json\": \"{}\"}}}}}}}}",
        SERVED.replace('"', "\\\"")
    )
}

/// The `x-amazon-apigateway-request-validators` map these definitions declare: one entry per
/// flag combination under test, written out by hand so the document's own key order is what
/// the import walks.
fn declared_validators() -> String {
    format!(
        "{{\"{BODY_ONLY}\": {{\"validateRequestBody\": true, \
             \"validateRequestParameters\": false}}, \
           \"{PARAMETERS_ONLY}\": {{\"validateRequestBody\": false, \
             \"validateRequestParameters\": true}}, \
           \"{BOTH}\": {{\"validateRequestBody\": true, \
             \"validateRequestParameters\": true}}}}"
    )
}

/// The schemas these definitions name: an object requiring one NUMBER property, and the
/// sibling it references, so the stored schema's own reference is under test as well.
fn declared_schemas() -> String {
    format!(
        "{{\"{MODEL}\": {{\"type\": \"object\", \"required\": [\"weight\"], \
             \"properties\": {{\"weight\": {{\"type\": \"number\"}}, \
               \"note\": {{\"$ref\": \"#/{{SCHEMAS}}/{NESTED_MODEL}\"}}}}}}, \
           \"{NESTED_MODEL}\": {{\"type\": \"string\"}}}}"
    )
}

/// Which spelling a definition under test is written in, which decides how it names its
/// schemas and how an operation declares its body.
#[derive(Debug, Clone, Copy)]
enum Spelling {
    /// Swagger 2.0: `definitions`, and a `body` parameter carrying a bare `schema`.
    Swagger,
    /// OpenAPI 3: `components.schemas`, and `requestBody.content` keyed by media type.
    OpenApi,
}

impl Spelling {
    /// The JSON pointer prefix this spelling names a declared schema under.
    fn schemas_pointer(self) -> &'static str {
        match self {
            Spelling::Swagger => "definitions",
            Spelling::OpenApi => "components/schemas",
        }
    }

    /// The operation object for a path, selecting `validator` where it overrides the
    /// document's own, and declaring a body that references [`MODEL`].
    fn operation(self, validator: Option<&str>) -> String {
        let selector = validator
            .map(|name| format!("\"x-amazon-apigateway-request-validator\": \"{name}\", "))
            .unwrap_or_default();
        let reference = format!(
            "{{\"$ref\": \"#/{}/{MODEL}\"}}",
            self.schemas_pointer()
        );
        let body = match self {
            Spelling::Swagger => format!(
                "\"parameters\": [{{\"in\": \"body\", \"name\": \"body\", \
                   \"schema\": {reference}}}], "
            ),
            Spelling::OpenApi => format!(
                "\"requestBody\": {{\"content\": \
                   {{\"application/json\": {{\"schema\": {reference}}}}}}}, "
            ),
        };
        format!(
            "{{{selector}{body}\"responses\": {{\"200\": {{\"description\": \"ok\"}}}}, \
               \"x-amazon-apigateway-integration\": {}}}",
            mock_integration()
        )
    }

    /// The whole definition: the declared validators, the document's own selector, the named
    /// schemas, and three paths — one on the document's validator, one overriding it, and one
    /// selecting a name the document does not declare.
    fn definition(self, title: &str) -> String {
        let schemas = declared_schemas().replace("{SCHEMAS}", self.schemas_pointer());
        let paths = format!(
            "{{\"/strict\": {{\"post\": {}}}, \
               \"/loose\": {{\"post\": {}}}, \
               \"/open\": {{\"post\": {}}}}}",
            self.operation(None),
            self.operation(Some(PARAMETERS_ONLY)),
            self.operation(Some(UNDECLARED)),
        );
        let head = match self {
            Spelling::Swagger => "\"swagger\": \"2.0\"".to_string(),
            Spelling::OpenApi => "\"openapi\": \"3.0.1\"".to_string(),
        };
        let schema_section = match self {
            Spelling::Swagger => format!("\"definitions\": {schemas}"),
            Spelling::OpenApi => format!("\"components\": {{\"schemas\": {schemas}}}"),
        };
        format!(
            "{{{head}, \
               \"info\": {{\"title\": \"{title}\", \"version\": \"2026-01-01\"}}, \
               \"x-amazon-apigateway-request-validators\": {}, \
               \"x-amazon-apigateway-request-validator\": \"{BODY_ONLY}\", \
               {schema_section}, \
               \"paths\": {paths}}}",
            declared_validators()
        )
    }
}

/// Every request validator of `api`, keyed by the name it reports.
async fn validators_by_name(gateway: &Gateway, api: &str, case: &str) -> Vec<(String, Value)> {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/requestvalidators"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetRequestValidators expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    page_items(&answer.body, case)
        .into_iter()
        .map(|item| (string_member(&item, "name", case), item))
        .collect()
}

/// Every model name of `api`, in the order the listing reports them.
async fn model_names(gateway: &Gateway, api: &str, case: &str) -> Vec<String> {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/models"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetModels expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    page_items(&answer.body, case)
        .into_iter()
        .map(|item| string_member(&item, "name", case))
        .collect()
}

/// One model of `api`, by name.
async fn model(gateway: &Gateway, api: &str, name: &str, case: &str) -> Value {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/models/{name}"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetModel({name}) expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `Method` at `path`'s resource under `verb`.
async fn method_at(gateway: &Gateway, api: &str, path: &str, verb: &str, case: &str) -> Value {
    let resources = gateway
        .call("GET", &format!("/restapis/{api}/resources"), None)
        .await;
    assert_eq!(
        resources.status, 200,
        "case {case}: GetResources expected 200, got {} with {:?}",
        resources.status, resources.body
    );
    let resource = page_items(&resources.body, case)
        .into_iter()
        .find(|item| string_member(item, "path", case) == path)
        .unwrap_or_else(|| panic!("case {case}: expected a resource at {path:?}"));
    let id = string_member(&resource, "id", case);
    let answer = gateway
        .call(
            "GET",
            &format!("/restapis/{api}/resources/{id}/methods/{verb}"),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetMethod({path}/{verb}) expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// One answered call of the deployed stage: the status and the body verbatim.
///
/// The sibling data-plane harness sends no payload, and the calls here are POSTs of a body
/// the method's validator decides on, so this suite builds its own request through the
/// crate's one emulator seam ([`Gateway::send`]) and reads the raw answer.
struct Served {
    status: u16,
    body: String,
}

impl Served {
    /// The `message` member of a JSON body, which is what every API Gateway gateway response
    /// carries.
    fn message(&self) -> Option<String> {
        serde_json::from_str::<Value>(&self.body)
            .ok()?
            .get("message")?
            .as_str()
            .map(str::to_owned)
    }
}

/// One POST of `body` to `path` inside the deployed stage of `api`.
async fn post(gateway: &Gateway, api: &str, path: &str, body: &str) -> Served {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/{STAGE}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .body(::bytes::Bytes::from(body.to_owned()))
        .expect("the invoke request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    Served {
        status,
        body: String::from_utf8_lossy(&response.into_body()).into_owned(),
    }
}

/// RULE: every declared validator becomes a record of the imported api, the selector in force
/// decides each method's `requestValidatorId`, every named schema becomes a model the declared
/// bodies reference — and the deployed stage then refuses a body the referenced model does not
/// admit while the parameters-only twin serves it.
#[tokio::test]
async fn an_import_creates_the_validators_and_models_its_definition_declares() {
    // Rows differ along the spelling the definition is written in, which names its schemas
    // and its bodies differently.
    for spelling in [Spelling::Swagger, Spelling::OpenApi] {
        let case = format!("{spelling:?}");
        let gateway = Gateway::new();
        let answer = import(&gateway, &spelling.definition(&case), true, &[]).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: ImportRestApi expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let api = string_member(&answer.body, "id", &case);

        // One validator per declared entry, named by the entry's key and carrying its flags.
        let validators = validators_by_name(&gateway, &api, &case).await;
        let declared: Vec<&str> = validators
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let mut sorted = declared.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![BODY_ONLY, BOTH, PARAMETERS_ONLY],
            "case {case}: expected one validator per declared entry, got {declared:?}"
        );
        // The flags each entry declared, with no entry's flags standing in for another's.
        let flags: &[(&str, bool, bool)] = &[
            (BODY_ONLY, true, false),
            (PARAMETERS_ONLY, false, true),
            (BOTH, true, true),
        ];
        for (name, body, parameters) in flags {
            let (_, reported) = validators
                .iter()
                .find(|(candidate, _)| candidate == name)
                .unwrap_or_else(|| panic!("case {case}: expected a validator named {name:?}"));
            assert_eq!(
                reported.get("validateRequestBody").and_then(Value::as_bool),
                Some(*body),
                "case {case}: expected validateRequestBody {body} for {name}, got {reported:?}"
            );
            assert_eq!(
                reported
                    .get("validateRequestParameters")
                    .and_then(Value::as_bool),
                Some(*parameters),
                "case {case}: expected validateRequestParameters {parameters} for {name}, got \
                 {reported:?}"
            );
        }

        // Every named schema is a model of the api, beside the two every api is created with.
        let models = model_names(&gateway, &api, &case).await;
        for named in [MODEL, NESTED_MODEL, "Empty", "Error"] {
            assert!(
                models.iter().any(|held| held == named),
                "case {case}: expected a model named {named:?}, got {models:?}"
            );
        }
        // The stored schema is the JSON the document declared it as, with its reference to a
        // sibling schema rewritten onto that sibling's model-schema URL — the identifier a
        // stored model's schema is resolved against.
        let stored = model(&gateway, &api, MODEL, &case).await;
        let schema = stored
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let parsed: Value = serde_json::from_str(&schema)
            .unwrap_or_else(|error| panic!("case {case}: the stored schema is JSON: {error}"));
        assert_eq!(
            parsed.get("required"),
            Some(&json!(["weight"])),
            "case {case}: expected the declared required list, got {parsed:?}"
        );
        assert_eq!(
            parsed
                .pointer("/properties/weight/type")
                .and_then(Value::as_str),
            Some("number"),
            "case {case}: expected the declared property type, got {parsed:?}"
        );
        assert_eq!(
            parsed.pointer("/properties/note/$ref").and_then(Value::as_str),
            Some(
                format!(
                    "https://apigateway.amazonaws.com/restapis/{api}/models/{NESTED_MODEL}"
                )
                .as_str()
            ),
            "case {case}: expected the sibling reference rewritten onto its model, got {parsed:?}"
        );

        // Which validator each method names: the document's own, the operation's override, and
        // none for a selector naming no declared entry.
        let by_id: Vec<(String, String)> = validators
            .iter()
            .map(|(name, reported)| (name.clone(), string_member(reported, "id", &case)))
            .collect();
        let id_of = |name: &str| -> String {
            by_id
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| panic!("case {case}: expected an id for {name:?}"))
        };
        let selected: &[(&str, Option<String>)] = &[
            ("/strict", Some(id_of(BODY_ONLY))),
            ("/loose", Some(id_of(PARAMETERS_ONLY))),
            ("/open", None),
        ];
        for (path, expected) in selected {
            let method = method_at(&gateway, &api, path, "POST", &case).await;
            assert_eq!(
                method
                    .get("requestValidatorId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                *expected,
                "case {case}: expected {path} to name {expected:?}, got {method:?}"
            );
            // And the model its declared body references, under the media type it was
            // declared for.
            assert_eq!(
                method.pointer("/requestModels/application~1json"),
                Some(&json!(MODEL)),
                "case {case}: expected {path} to map the declared model, got {method:?}"
            );
        }

        // The stage then applies the validator in force before the integration runs.
        deploy(&gateway, &api, STAGE).await;
        // Rows differ along the validator in force and whether the body satisfies the model.
        let calls: &[(&str, &str, &str, u16, &str)] = &[
            (
                "a body the model does not admit, on the body-checking method",
                "/strict",
                r#"{"note":"no weight here"}"#,
                400,
                INVALID_REQUEST_BODY,
            ),
            (
                "a body the model admits, on the same method",
                "/strict",
                r#"{"weight":2.5}"#,
                200,
                SERVED,
            ),
            (
                "the identical refused body, on the parameters-only twin",
                "/loose",
                r#"{"note":"no weight here"}"#,
                200,
                SERVED,
            ),
            (
                "the same body on the method whose selector names no declared validator",
                "/open",
                r#"{"note":"no weight here"}"#,
                200,
                SERVED,
            ),
        ];
        for (row, path, body, status, expected) in calls {
            let served = post(&gateway, &api, path, body).await;
            assert_eq!(
                served.status, *status,
                "case {case}/{row}: expected {status}, got {} with {:?}",
                served.status, served.body
            );
            if *status == 200 {
                assert_eq!(
                    served.body, *expected,
                    "case {case}/{row}: expected the integration's body, got {:?}",
                    served.body
                );
            } else {
                assert_eq!(
                    served.message().as_deref(),
                    Some(*expected),
                    "case {case}/{row}: expected the bad-request-body sentence, got {:?}",
                    served.body
                );
            }
        }
    }
}
