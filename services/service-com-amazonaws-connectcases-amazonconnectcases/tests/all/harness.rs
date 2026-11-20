// SPDX-License-Identifier: CC0-1.0

//! The shared end-to-end harness for this crate's suites: one emulator behind
//! the real `MultiServiceRouter`, plus the small helpers every suite needs to
//! drive the Cases lifecycle (domain -> field -> template -> case) over the
//! wire the SDK speaks.
//!
//! It lives in its own module so a new suite reuses it instead of growing a
//! second copy of the same request plumbing.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::ops::types::FIELD_TYPE_VALUES;
use service_com_amazonaws_connectcases_amazonconnectcases::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The access key id the harness signs with unless a suite names another one.
pub const DEFAULT_ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// A SigV4 `Authorization` header scoped to this service's signing name
/// (`cases`, per the crate's `OPERATIONS.md`), which is what the router routes
/// on and what the handlers read the ARN region from, signed by
/// `access_key_id` — the id a handler attributes a stored record to, so a suite
/// that needs two callers told apart names a second one.
pub fn authorization_as(access_key_id: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/cases/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One emulator: a single shared engine behind the production router.
pub struct Emulator {
    router: MultiServiceRouter,
    region: String,
}

impl Emulator {
    pub fn new() -> Self {
        Self::in_region("us-east-1")
    }

    pub fn in_region(region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self {
            router,
            region: region.to_string(),
        }
    }

    pub async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.send_as(DEFAULT_ACCESS_KEY_ID, method, path, body)
            .await
    }

    /// Send a call signed by `access_key_id` rather than [`DEFAULT_ACCESS_KEY_ID`].
    pub async fn send_as(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let payload = body
            .map(|body| serde_json::to_vec(&body).expect("serialize the request body"))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://cases.{}.amazonaws.com{path}", self.region))
            .header("authorization", authorization_as(access_key_id, &self.region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let json = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, json)
    }

    /// Send a call that must succeed, and hand back its parsed body.
    pub async fn ok(&self, case: &str, method: &str, path: &str, body: Value) -> Value {
        self.ok_as(DEFAULT_ACCESS_KEY_ID, case, method, path, body)
            .await
    }

    /// Send a call signed by `access_key_id` that must succeed, and hand back
    /// its parsed body.
    pub async fn ok_as(
        &self,
        access_key_id: &str,
        case: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> Value {
        let (status, json) = self.send_as(access_key_id, method, path, Some(body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {json:?}"
        );
        json
    }

    /// Send a call that must fail, and hand back its status and wire error code.
    pub async fn failure(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String) {
        let (status, json) = self.send(method, path, body).await;
        let code = json
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .to_string();
        assert_ne!(
            status, 200,
            "case {case:?}: expected a failure, got HTTP 200 with body {json:?}"
        );
        (status, code)
    }

    /// Send a call that must fail, and hand back its status, wire error code and message.
    pub async fn failure_detail(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String, String) {
        let (status, json) = self.send(method, path, body).await;
        assert_ne!(
            status, 200,
            "case {case:?}: expected a failure, got HTTP 200 with body {json:?}"
        );
        let code = json
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .to_string();
        let message = json
            .get("message")
            .or_else(|| json.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string();
        (status, code, message)
    }
}

/// Percent-encode one path segment, so an ARN travels in the non-greedy `{arn}`
/// label of `/tags/{arn}`.
fn label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The tagging path of one resource.
pub fn tags_path(arn: &str) -> String {
    format!("/tags/{}", label(arn))
}

pub fn string_at(json: &Value, member: &str, case: &str) -> String {
    json.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no string member {member:?} in {json:?}"))
        .to_string()
}

/// The instant the `date-time` member `member` names, in epoch seconds.
///
/// Every timestamp member of this model declares
/// `@timestampFormat("date-time")`, so the service reports an instant as an RFC
/// 3339 string; this is the one place a suite turns that text back into a number
/// it can order and compare.
pub fn instant_at(json: &Value, member: &str, case: &str) -> f64 {
    let text = json
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: no date-time member {member:?} in {json:?}")
        });
    ::aws_smithy_types::DateTime::from_str(text, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {member} {text:?} is not an RFC 3339 date-time: {error}")
        })
        .as_secs_f64()
}

/// Every `FieldValueUnion` variant, each paired with the `FieldType` whose
/// values it carries. The model gives a case field's value the field's own
/// declared type (`FieldValueUnion` in `src/ops/types/shared_types.rs`,
/// `FieldType` in `FIELD_TYPE_VALUES`): `booleanValue` is a Boolean, a
/// `doubleValue` a Number, a `userArnValue` a User, and the string-valued types
/// take `stringValue`. `emptyValue` is the absent value and fits any type — the
/// row below writes it against a `SingleSelect` field. Each row is a value whose
/// stored form a read must reproduce exactly, variant included.
pub fn field_value_variants() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "stringValue",
            "Text",
            json!({ "stringValue": "a customer note" }),
        ),
        ("doubleValue", "Number", json!({ "doubleValue": 42.5 })),
        ("booleanValue", "Boolean", json!({ "booleanValue": true })),
        ("emptyValue", "SingleSelect", json!({ "emptyValue": {} })),
        (
            "userArnValue",
            "User",
            json!({ "userArnValue": "arn:aws:connect:us-east-1:000000000000:instance/i-1/agent/a-1" }),
        ),
    ]
}

/// The id of the field of `field_type` among `fields`, as
/// `fields_of_every_type` created them.
pub fn field_of_type(fields: &[(&'static str, String)], field_type: &str) -> String {
    fields
        .iter()
        .find(|(created, _)| *created == field_type)
        .map(|(_, id)| id.clone())
        .unwrap_or_else(|| panic!("no {field_type} field was created: {fields:?}"))
}

/// Create a domain and return its id.
pub async fn domain(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .ok(name, "POST", "/domains", json!({ "name": name }))
        .await;
    string_at(&created, "domainId", name)
}

/// Create one field per `FieldType` the model declares, returning
/// `(type, fieldId)` in that order.
pub async fn fields_of_every_type(
    emulator: &Emulator,
    domain_id: &str,
) -> Vec<(&'static str, String)> {
    let mut created = Vec::new();
    for field_type in FIELD_TYPE_VALUES {
        let case = format!("CreateField type={field_type}");
        let body = json!({ "name": format!("field-{field_type}"), "type": field_type });
        let response = emulator
            .ok(&case, "POST", &format!("/domains/{domain_id}/fields"), body)
            .await;
        let field_id = string_at(&response, "fieldId", &case);
        assert!(
            is_minted_id(&field_id),
            "case {case:?}: the fieldId must be a server-minted opaque id, got {field_id:?}"
        );
        assert_eq!(
            string_at(&response, "fieldArn", &case),
            format!("arn:aws:cases:us-east-1:000000000000:domain/{domain_id}/field/{field_id}"),
            "case {case:?}: fieldArn must address the stored field, got {response:?}"
        );
        created.push((*field_type, field_id));
    }
    created
}

/// `BatchGetField` over `ids`, as its `(fields, errors)` lists.
pub async fn batch_get_fields(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    ids: &[String],
) -> (Vec<Value>, Vec<Value>) {
    let requested: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
    let response = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/fields-batch"),
            json!({ "fields": requested }),
        )
        .await;
    let fields = response
        .get("fields")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: BatchGetField reported no fields: {response:?}"))
        .clone();
    let errors = response
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    (fields, errors)
}

/// The one field `BatchGetField` reports for `field_id`, which must resolve.
pub async fn read_field(emulator: &Emulator, case: &str, domain_id: &str, field_id: &str) -> Value {
    let ids = vec![field_id.to_string()];
    let (fields, errors) = batch_get_fields(emulator, case, domain_id, &ids).await;
    assert!(
        errors.is_empty(),
        "case {case:?}: BatchGetField must still retrieve {field_id}, got errors {errors:?}"
    );
    assert_eq!(
        fields.len(),
        1,
        "case {case:?}: expected exactly one field back for {field_id}, got {fields:?}"
    );
    fields[0].clone()
}

/// Every field summary `ListFields` reports for `domain_id`, following its
/// pages to exhaustion so the answer is every live field and not only the first
/// page. `nextToken` is an `@httpQuery` member of this operation, so the cursor
/// travels in the query string rather than in the body.
pub async fn listed_fields(emulator: &Emulator, case: &str, domain_id: &str) -> Vec<Value> {
    let mut listed: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let query = match &token {
            Some(token) => format!("?nextToken={token}"),
            None => String::new(),
        };
        let answer = emulator
            .ok(
                case,
                "POST",
                &format!("/domains/{domain_id}/fields-list{query}"),
                json!({}),
            )
            .await;
        let page = answer
            .get("fields")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: ListFields reported no fields: {answer:?}"))
            .clone();
        listed.extend(page);
        token = answer
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return listed;
        }
    }
}

/// Create a template requiring `field_ids`, returning its id.
pub async fn template(
    emulator: &Emulator,
    domain_id: &str,
    name: &str,
    field_ids: &[String],
) -> String {
    let required: Vec<Value> = field_ids
        .iter()
        .map(|id| json!({ "fieldId": id }))
        .collect();
    let response = emulator
        .ok(
            name,
            "POST",
            &format!("/domains/{domain_id}/templates"),
            json!({ "name": name, "requiredFields": required }),
        )
        .await;
    string_at(&response, "templateId", name)
}

/// `GetCase` for `ids`, returning the `fields` list verbatim.
pub async fn get_case_fields(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    case_id: &str,
    ids: &[String],
) -> Vec<Value> {
    let requested: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
    let response = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": requested }),
        )
        .await;
    response
        .get("fields")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: GetCase returned no fields list: {response:?}"))
        .clone()
}

/// `true` when `id` is a server-minted resource id: the 8-4-4-4-12 lowercase
/// hex form the real service's ids take, carrying nothing from the request.
pub fn is_minted_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}
