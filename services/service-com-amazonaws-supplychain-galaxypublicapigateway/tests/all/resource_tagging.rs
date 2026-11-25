// SPDX-License-Identifier: CC0-1.0

//! WHO may tag a resource of this service, and what a tag write persists — driven end to
//! end over the real wire path (a restJson1 request through the multi-service router into
//! the mounted service), so what is asserted is what a client holding those credentials
//! sees.
//!
//! RULES under test:
//!
//! * every tagging operation evaluates the identity-based IAM policies in force on the
//!   principal its credentials name against `scn:<OperationName>`, on the resource the
//!   request ADDRESSED, BEFORE it reads or writes anything — so a request no statement
//!   allows (IAM's default deny), or one an explicit `Deny` covers, is answered HTTP 403
//!   `AccessDeniedException`, no tag is written, and nothing about the resource is
//!   disclosed; a request a matching `Allow` covers reaches the operation's own answer;
//! * a tag write persists onto the STORED record of the resource the ARN names, whatever
//!   kind of resource that is, and a later read reports exactly what was written: the keys
//!   a write carried are added or replaced and the ones it did not name are left alone;
//! * a tag removal takes off the stored record exactly the keys it NAMES, whatever their
//!   values were, and leaves every other tag of that resource alone; a key the resource
//!   does not carry takes nothing off and is still a success;
//! * a well-formed `AscResourceArn` naming a resource the store does not hold is a
//!   `ResourceNotFoundException` (404) rather than a tag held against nothing.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME
//! shared engine — the stores those services' own handlers write — so the documents under
//! test are the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the
//! one `sts:GetCallerIdentity` answers for the key the request signed with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every ARN this emulator mints carries
/// (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The access key id of the assumed-role session the authorization cases sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, and the session's own name.
const CALLER_ROLE: &str = "scn-tagger";
const CALLER_SESSION: &str = "scn-session";

/// The name of the inline policy a case puts in force on that role.
const POLICY_NAME: &str = "inline";

/// How long a seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The IAM action namespace this service publishes its operations under: its `arnNamespace`
/// and SigV4 signing name
/// (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`).
const ACTION_PREFIX: &str = "scn";

/// The not-found sentences this service answers a tag call addressing a resource it does
/// not hold with, written out HERE rather than read from the crate: the wording is the
/// requirement, so a drift in the constant the handler answers from fails this suite
/// instead of travelling with it.
const INSTANCE_NOT_FOUND_SENTENCE: &str = "Instance not found.";

/// The namespace half of that family names the namespace the request addressed: its SOURCE
/// is the recorded wire response of a namespace read, whose fixed text is "Namespace " /
/// " not exist" with the name from the request.
fn namespace_not_found_sentence(name: &str) -> String {
    format!("Namespace {name} not exist")
}

/// This service beside the IAM and STS services on one shared engine, so the policies and
/// the session a case seeds are the ones those services hold.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise operation behaviour, not request signing.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Send one request signed with `access_key_id` — what names the principal the request
    /// speaks for — and return its HTTP status and JSON body.
    async fn send_as(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{ACTION_PREFIX}/\
             aws4_request, SignedHeaders=host, Signature=00"
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

    /// Send one request as the account's own published credential.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.send_as(ACCOUNT_KEY_ID, method, path, body).await
    }

    /// A live instance of this account, created through `CreateInstance`.
    async fn given_instance(&self, name: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/instance", Some(json!({ "instanceName": name })))
            .await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body["instance"]["instanceId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// A namespace of `instance_id`, created through `CreateDataLakeNamespace`.
    async fn given_namespace(&self, instance_id: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeNamespace expected 200, got {status} with {body}"
        );
    }

    /// A dataset of `instance_id`'s `namespace`, created through `CreateDataLakeDataset`.
    async fn given_dataset(&self, instance_id: &str, namespace: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!(
                    "/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}"
                ),
                Some(json!({
                    "schema": {
                        "name": "Shipment",
                        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeDataset expected 200, got {status} with {body}"
        );
    }

    /// `TagResource` for `resource_arn`, signed with `access_key_id`.
    async fn tag_as(
        &self,
        access_key_id: &str,
        resource_arn: &str,
        tags: Value,
    ) -> (u16, Value) {
        self.send_as(
            access_key_id,
            "POST",
            &format!("/api/tags/{}", encoded(resource_arn)),
            Some(json!({ "tags": tags })),
        )
        .await
    }

    /// `UntagResource` for `resource_arn`, signed with `access_key_id`.
    ///
    /// `tagKeys` is an `@httpQuery("tagKeys")` list
    /// (`smithy_query(query="operation", operation="UntagResource", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`),
    /// so every key travels as its own `tagKeys=` parameter of the DELETE.
    async fn untag_as(
        &self,
        access_key_id: &str,
        resource_arn: &str,
        tag_keys: &[String],
    ) -> (u16, Value) {
        let query: Vec<String> = tag_keys
            .iter()
            .map(|key| format!("tagKeys={}", query_encoded(key)))
            .collect();
        let mut path = format!("/api/tags/{}", encoded(resource_arn));
        if !query.is_empty() {
            path.push('?');
            path.push_str(&query.join("&"));
        }
        self.send_as(access_key_id, "DELETE", &path, None).await
    }

    /// `ListTagsForResource` for `resource_arn`, signed with `access_key_id`.
    async fn list_tags_as(&self, access_key_id: &str, resource_arn: &str) -> (u16, Value) {
        self.send_as(
            access_key_id,
            "GET",
            &format!("/api/tags/{}", encoded(resource_arn)),
            None,
        )
        .await
    }

    /// The tags `ListTagsForResource` reports for `resource_arn`, asserting the 200.
    async fn tags_of(&self, case: &str, resource_arn: &str) -> BTreeMap<String, String> {
        let (status, body) = self.list_tags_as(ACCOUNT_KEY_ID, resource_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTagsForResource expected 200, got {status} with {body}"
        );
        let tags = body
            .get("tags")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("case {case:?}: the answer carries no `tags` map: {body}"));
        tags.iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: a tag value is not a string"))
                        .to_string(),
                )
            })
            .collect()
    }
}

/// An ARN as ONE URI path segment: `{resourceArn}` is a non-greedy `@httpLabel`, so the
/// `:` and `/` of an ARN travel as `%3A` and `%2F` and the whole ARN stays one segment.
fn encoded(arn: &str) -> String {
    arn.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// One query-parameter VALUE. `tagKeys` is `@httpQuery`, so a tag key carrying a space or
/// one of the separators a query string gives meaning to (`&`, `=`, `/`, `:`) reaches the
/// server only percent-encoded; the encoding is the maintained `percent-encoding` crate's,
/// over everything outside the alphanumerics, which is what an SDK does to a query value.
fn query_encoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The ARN of one instance, as this service mints it.
fn instance_arn(instance_id: &str) -> String {
    format!("arn:aws:{ACTION_PREFIX}:{REGION}:{ACCOUNT}:instance/{instance_id}")
}

/// The error code a refusal envelope carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal envelope carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The principal ARN an assumed-role session of [`CALLER_ROLE`] speaks for — what an
/// authorization refusal names.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{CALLER_ROLE}/{CALLER_SESSION}")
}

/// The ARN the account holds [`CALLER_ROLE`] under.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{CALLER_ROLE}")
}

/// Record [`CALLER_ROLE`] and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    store.lock().roles.insert(
        iam::name_key(CALLER_ROLE),
        service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: CALLER_ROLE.to_string(),
            role_id: format!("AROAEXAMPLE{}", CALLER_ROLE.to_uppercase()),
            arn: role_arn(),
            create_date: aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn()),
                account: ACCOUNT.to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let sts_store = sts::service_state(&**engine).expect("the STS store");
    sts_store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Put `document` in force on [`CALLER_ROLE`] as its inline policy, replacing whatever it
/// carried; `None` takes every policy off it, as `iam:DeleteRolePolicy` does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store
        .role_inline_policies
        .remove(&iam::name_key(CALLER_ROLE));
    if let Some(document) = document {
        store.role_inline_policies.insert(
            iam::name_key(CALLER_ROLE),
            BTreeMap::from([(
                iam::name_key(POLICY_NAME),
                iam::InlinePolicy {
                    policy_name: POLICY_NAME.to_string(),
                    policy_document: document.to_string(),
                },
            )]),
        );
    }
}

/// A policy allowing `actions` on `resource`.
fn allow(actions: Value, resource: Value) -> Value {
    json!({ "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }] })
}

/// Which of IAM's two refusals a row expects.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The default deny of a request no statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// The sentence IAM refuses a request with: who, which action, on which resource, and
/// which of its two rules refused.
fn not_authorized(principal: &str, action: &str, resource: &str, refusal: Refusal) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {}",
        refusal.reason(action)
    )
}

/// The documents a row puts in force on the calling role, and which of IAM's refusals the
/// request then meets — `None` for a request a matching `Allow` permits.
fn documents_in_force(action: &str, arn: &str) -> Vec<(&'static str, Option<Value>, Option<Refusal>)> {
    vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an allow of another action of this service, but not this one",
            Some(allow(json!("scn:CreateInstance"), json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of every action of another service",
            Some(allow(json!("s3:*"), json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of this action on ANOTHER resource",
            Some(allow(json!(action), json!(format!("{arn}-not-this-one")))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of this whole service beside an allow of exactly this action",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": action, "Resource": "*" },
                    { "Effect": "Deny", "Action": "scn:*", "Resource": "*" },
                ],
            })),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "an allow of exactly this action on every resource",
            Some(allow(json!(action), json!("*"))),
            None,
        ),
        (
            // IAM compares action names case-insensitively, so the spelling of the allow
            // is not what decides the request.
            "an allow of this action spelled in lower case",
            Some(allow(json!(action.to_lowercase()), json!("*"))),
            None,
        ),
        (
            "an allow of this action on exactly this resource",
            Some(allow(json!(action), json!(arn))),
            None,
        ),
        (
            "an allow of every action of this service",
            Some(allow(json!("scn:*"), json!("*"))),
            None,
        ),
        ("an allow of every action", Some(allow(json!("*"), json!("*"))), None),
    ]
}

/// RULE: the tag write is decided against the calling principal's identity-based policies
/// before anything is written, so a principal no readable document grants
/// `scn:TagResource` to is answered 403 with IAM's own sentence and the resource keeps the
/// tags it had; a principal a matching `Allow` covers is served.
#[tokio::test]
async fn a_tag_write_is_decided_against_the_callers_identity_policies() {
    let action = format!("{ACTION_PREFIX}:TagResource");
    let rows = documents_in_force(&action, "arn:aws:scn:us-east-1:000000000000:instance/x").len();
    for row in 0..rows {
        let emulator = Emulator::new();
        given_session(&emulator.engine);
        let instance_id = emulator.given_instance("tag-authorization").await;
        let arn = instance_arn(&instance_id);

        let mut in_force = documents_in_force(&action, &arn);
        let (case, document, refusal) = in_force.remove(row);
        given_role_policy(&emulator.engine, document.as_ref());

        let (status, body) = emulator
            .tag_as(SESSION_KEY_ID, &arn, json!({ "team": "sim2real" }))
            .await;
        match refusal {
            Some(refusal) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (403, "AccessDeniedException"),
                    "case {case:?}: expected 403 AccessDeniedException, got {status} with {body}"
                );
                assert_eq!(
                    error_message(&body),
                    not_authorized(&session_arn(), &action, &arn, refusal),
                    "case {case:?}: expected IAM's refusal wording, got {body}"
                );
                assert!(
                    emulator.tags_of(case, &arn).await.is_empty(),
                    "case {case:?}: a refused tag write must persist nothing"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an authorized caller is served, got {status} with {body}"
                );
                assert_eq!(
                    emulator.tags_of(case, &arn).await,
                    BTreeMap::from([("team".to_string(), "sim2real".to_string())]),
                    "case {case:?}: an authorized tag write must persist"
                );
            }
        }
    }
}

/// RULE: the same gate decides the tag READ, under that operation's own action name — so a
/// principal allowed to write tags but not to read them is still refused the read, and is
/// told nothing about the resource.
#[tokio::test]
async fn a_tag_read_is_decided_under_its_own_action_name() {
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let instance_id = emulator.given_instance("tag-read-authorization").await;
    let arn = instance_arn(&instance_id);
    let read_action = format!("{ACTION_PREFIX}:ListTagsForResource");

    // Rows differ in WHICH action the one document in force allows.
    for (case, allowed, refused) in [
        ("an allow of the write alone", "scn:TagResource", true),
        ("an allow of the read alone", "scn:ListTagsForResource", false),
        ("an allow of every action of this service", "scn:*", false),
    ] {
        given_role_policy(&emulator.engine, Some(&allow(json!(allowed), json!("*"))));
        let (status, body) = emulator.list_tags_as(SESSION_KEY_ID, &arn).await;
        if refused {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: expected 403 AccessDeniedException, got {status} with {body}"
            );
            assert_eq!(
                error_message(&body),
                not_authorized(&session_arn(), &read_action, &arn, Refusal::DefaultDeny),
                "case {case:?}: expected IAM's refusal wording, got {body}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: an authorized read is served, got {status} with {body}"
            );
        }
    }
}

/// RULE: a tag write persists onto the stored record of the resource the ARN names —
/// whatever kind of resource it is — adding or replacing exactly the keys it carried and
/// leaving the others alone, and a later read reports what was written.
#[tokio::test]
async fn tags_persist_on_every_kind_of_resource_an_arn_names() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance("tag-round-trip").await;
    emulator.given_namespace(&instance_id, "orders").await;
    emulator
        .given_dataset(&instance_id, "default", "shipments")
        .await;

    // Rows: one per kind of resource an `AscResourceArn` of this service names, as the
    // helpers that mint them spell the resource portion.
    let rows = [
        ("the instance itself", instance_arn(&instance_id)),
        (
            "a data lake namespace",
            format!("{}/namespaces/orders", instance_arn(&instance_id)),
        ),
        (
            "a data lake dataset",
            format!(
                "{}/namespaces/default/datasets/shipments",
                instance_arn(&instance_id)
            ),
        ),
    ];

    for (case, arn) in rows {
        assert!(
            emulator.tags_of(case, &arn).await.is_empty(),
            "case {case:?}: a resource created with no tags must report none"
        );

        let (status, body) = emulator
            .tag_as(
                ACCOUNT_KEY_ID,
                &arn,
                json!({ "team": "sim2real", "tier": "dev" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: TagResource expected 200, got {status} with {body}"
        );
        assert_eq!(
            emulator.tags_of(case, &arn).await,
            BTreeMap::from([
                ("team".to_string(), "sim2real".to_string()),
                ("tier".to_string(), "dev".to_string()),
            ]),
            "case {case:?}: the read must report what the write persisted"
        );

        // A second write replaces the value of the key it names and adds the new one; the
        // key it does not name keeps the value it had.
        let (status, body) = emulator
            .tag_as(
                ACCOUNT_KEY_ID,
                &arn,
                json!({ "tier": "prod", "owner": "supply" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second TagResource expected 200, got {status} with {body}"
        );
        assert_eq!(
            emulator.tags_of(case, &arn).await,
            BTreeMap::from([
                ("team".to_string(), "sim2real".to_string()),
                ("tier".to_string(), "prod".to_string()),
                ("owner".to_string(), "supply".to_string()),
            ]),
            "case {case:?}: a write must merge into the tags the resource already carried"
        );

        // An empty tag map changes nothing: it names no key to add or replace.
        let (status, body) = emulator.tag_as(ACCOUNT_KEY_ID, &arn, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an empty TagResource expected 200, got {status} with {body}"
        );
        assert_eq!(
            emulator.tags_of(case, &arn).await.len(),
            3,
            "case {case:?}: an empty write must leave every stored tag alone"
        );
    }

    // Each resource was tagged independently: no write reached another resource's record.
    let namespace_arn = format!("{}/namespaces/orders", instance_arn(&instance_id));
    let (status, body) = emulator
        .tag_as(ACCOUNT_KEY_ID, &namespace_arn, json!({ "only": "here" }))
        .await;
    assert_eq!(
        status, 200,
        "case \"independence\": TagResource expected 200, got {status} with {body}"
    );
    assert!(
        !emulator
            .tags_of("independence", &instance_arn(&instance_id))
            .await
            .contains_key("only"),
        "case \"independence\": a namespace's tag must not appear on the instance"
    );
}

/// RULE: a well-formed `AscResourceArn` that names no resource this store holds is a
/// `ResourceNotFoundException` (404) — for the write and the read alike — and nothing is
/// stored against it.
#[tokio::test]
async fn an_arn_naming_no_stored_resource_is_a_not_found() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance("tag-not-found").await;
    let absent_instance = "00000000-0000-4000-8000-000000000000";

    // Rows differ along WHICH part of the ARN names nothing: the instance, the namespace
    // under a live instance, the dataset under a namespace it has, and an ARN of another
    // account. Each carries the sentence this service answers a read of THAT resource with.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an instance no store holds",
            instance_arn(absent_instance),
            INSTANCE_NOT_FOUND_SENTENCE.to_string(),
        ),
        (
            "a namespace that was never created",
            format!("{}/namespaces/never_created", instance_arn(&instance_id)),
            namespace_not_found_sentence("never_created"),
        ),
        (
            "a dataset that was never created",
            format!(
                "{}/namespaces/default/datasets/never_created",
                instance_arn(&instance_id)
            ),
            "Dataset never_created not exist in namespace default".to_string(),
        ),
        (
            "an instance ARN of another account",
            format!("arn:aws:{ACTION_PREFIX}:{REGION}:123456789012:instance/{instance_id}"),
            "Resource not found.".to_string(),
        ),
    ];

    for (case, arn, sentence) in rows {
        for (what, status, body) in [
            (
                "TagResource",
                emulator
                    .tag_as(ACCOUNT_KEY_ID, &arn, json!({ "team": "sim2real" }))
                    .await,
            ),
            (
                "ListTagsForResource",
                emulator.list_tags_as(ACCOUNT_KEY_ID, &arn).await,
            ),
            (
                "UntagResource",
                emulator
                    .untag_as(ACCOUNT_KEY_ID, &arn, &["team".to_string()])
                    .await,
            ),
        ]
        .map(|(what, (status, body))| (what, status, body))
        {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: {what} expected 404 ResourceNotFoundException, got {status} \
                 with {body}"
            );
            assert_eq!(
                error_message(&body),
                sentence,
                "case {case:?}: {what} must carry this service's not-found sentence, got {body}"
            );
        }
    }

    // The live instance is untouched by every refused call above.
    assert!(
        emulator
            .tags_of("the live instance", &instance_arn(&instance_id))
            .await
            .is_empty(),
        "case \"the live instance\": a refused tag call must persist nothing"
    );
}

/// The tags a removal starts from.
///
/// The keys differ in FORM — plain, carrying the separators a query string gives meaning
/// to, spaced, and at each published bound of `TagKey`'s length (`min: 1, max: 128`, from
/// `smithy_query(query="shape", shape="TagKey", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`) —
/// because the keys to remove travel in the query string while the keys to write travel in
/// the body, and the removal is BY KEY.
fn seeded_tags() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("team".to_string(), "sim2real".to_string()),
        ("tier".to_string(), "dev".to_string()),
        ("cost:center/sub&unit".to_string(), "17".to_string()),
        ("owner name".to_string(), "supply chain".to_string()),
        ("k".to_string(), String::new()),
        ("K".repeat(128), "the longest key the model admits".to_string()),
    ])
}

/// RULE: a tag removal takes off the stored record of the resource the ARN names exactly
/// the keys it NAMES, whatever their values were, and leaves every other tag of that
/// resource alone; a key the resource does not carry takes nothing off and is still a
/// success, so the operation can be replayed.
#[tokio::test]
async fn a_tag_removal_takes_off_only_the_keys_it_names() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance("untag-round-trip").await;
    emulator.given_namespace(&instance_id, "orders").await;
    emulator
        .given_dataset(&instance_id, "default", "shipments")
        .await;

    // One per kind of resource an `AscResourceArn` of this service names.
    let resources = [
        ("the instance itself", instance_arn(&instance_id)),
        (
            "a data lake namespace",
            format!("{}/namespaces/orders", instance_arn(&instance_id)),
        ),
        (
            "a data lake dataset",
            format!(
                "{}/namespaces/default/datasets/shipments",
                instance_arn(&instance_id)
            ),
        ),
    ];

    for (resource, arn) in resources {
        let seeded = seeded_tags();
        let (status, body) = emulator
            .tag_as(ACCOUNT_KEY_ID, &arn, json!(seeded.clone()))
            .await;
        assert_eq!(
            status, 200,
            "case {resource:?}: TagResource expected 200, got {status} with {body}"
        );

        // Rows differ along WHICH keys one removal names, and how many: one of several, a
        // key the resource never carried, the same key twice, keys whose form the query
        // string must escape, the keys at the length bounds, and every key that is left —
        // applied in order to the same resource, so each row also asserts what the rows
        // before it left behind.
        let rows: Vec<(&str, Vec<String>)> = vec![
            ("one key of several", vec!["tier".to_string()]),
            (
                "a key the resource does not carry",
                vec!["never-written".to_string()],
            ),
            (
                "the same key named twice",
                vec!["team".to_string(), "team".to_string()],
            ),
            (
                "keys whose form the query string must escape",
                vec![
                    "cost:center/sub&unit".to_string(),
                    "owner name".to_string(),
                ],
            ),
            (
                "the shortest and the longest key the model admits",
                vec!["k".to_string(), "K".repeat(128)],
            ),
            (
                "every key, when the resource carries none",
                seeded.keys().cloned().collect(),
            ),
        ];

        let mut expected = seeded.clone();
        for (row, keys) in rows {
            let case = format!("{resource}, {row}");
            let (status, body) = emulator.untag_as(ACCOUNT_KEY_ID, &arn, &keys).await;
            assert_eq!(
                status, 200,
                "case {case:?}: UntagResource expected 200, got {status} with {body}"
            );
            for key in &keys {
                expected.remove(key);
            }
            let reported = emulator.tags_of(&case, &arn).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the tags the removal did not name, got {reported:?}"
            );
        }
    }

    // A removal reaches only the resource its ARN names: the same key on another resource
    // of the same instance survives it.
    let namespace_arn = format!("{}/namespaces/orders", instance_arn(&instance_id));
    for arn in [&instance_arn(&instance_id), &namespace_arn] {
        let (status, body) = emulator
            .tag_as(ACCOUNT_KEY_ID, arn, json!({ "shared": "both" }))
            .await;
        assert_eq!(
            status, 200,
            "case \"independence\": TagResource expected 200, got {status} with {body}"
        );
    }
    let (status, body) = emulator
        .untag_as(ACCOUNT_KEY_ID, &namespace_arn, &["shared".to_string()])
        .await;
    assert_eq!(
        status, 200,
        "case \"independence\": UntagResource expected 200, got {status} with {body}"
    );
    assert_eq!(
        emulator
            .tags_of("independence", &instance_arn(&instance_id))
            .await,
        BTreeMap::from([("shared".to_string(), "both".to_string())]),
        "case \"independence\": a namespace's removal must not reach the instance's tags"
    );
    assert!(
        emulator
            .tags_of("independence", &namespace_arn)
            .await
            .is_empty(),
        "case \"independence\": the namespace's own key must be gone"
    );

    // A key one character past `TagKey`'s published maximum is outside what the model
    // admits, so the request is refused (400 `ValidationException`) and no tag is reached.
    let case = "a key one past the longest the model admits";
    let instance = instance_arn(&instance_id);
    let (status, body) = emulator
        .untag_as(ACCOUNT_KEY_ID, &instance, &["K".repeat(129)])
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} with {body}"
    );
    assert_eq!(
        emulator.tags_of(case, &instance).await,
        BTreeMap::from([("shared".to_string(), "both".to_string())]),
        "case {case:?}: a refused removal must take nothing off"
    );
}

/// RULE: the tag removal is decided against the calling principal's identity-based policies
/// under its OWN action name before anything is removed, so a principal no readable
/// document grants `scn:UntagResource` to is answered 403 with IAM's own sentence and the
/// resource keeps every tag it carried; a principal a matching `Allow` covers is served.
#[tokio::test]
async fn a_tag_removal_is_decided_against_the_callers_identity_policies() {
    let action = format!("{ACTION_PREFIX}:UntagResource");
    let rows = documents_in_force(&action, "arn:aws:scn:us-east-1:000000000000:instance/x").len();
    for row in 0..rows {
        let emulator = Emulator::new();
        given_session(&emulator.engine);
        let instance_id = emulator.given_instance("untag-authorization").await;
        let arn = instance_arn(&instance_id);
        let (status, body) = emulator
            .tag_as(ACCOUNT_KEY_ID, &arn, json!({ "team": "sim2real" }))
            .await;
        assert_eq!(
            status, 200,
            "row {row}: TagResource expected 200, got {status} with {body}"
        );

        let mut in_force = documents_in_force(&action, &arn);
        let (case, document, refusal) = in_force.remove(row);
        given_role_policy(&emulator.engine, document.as_ref());

        let (status, body) = emulator
            .untag_as(SESSION_KEY_ID, &arn, &["team".to_string()])
            .await;
        match refusal {
            Some(refusal) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (403, "AccessDeniedException"),
                    "case {case:?}: expected 403 AccessDeniedException, got {status} with {body}"
                );
                assert_eq!(
                    error_message(&body),
                    not_authorized(&session_arn(), &action, &arn, refusal),
                    "case {case:?}: expected IAM's refusal wording, got {body}"
                );
                assert_eq!(
                    emulator.tags_of(case, &arn).await,
                    BTreeMap::from([("team".to_string(), "sim2real".to_string())]),
                    "case {case:?}: a refused removal must take nothing off"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an authorized caller is served, got {status} with {body}"
                );
                assert!(
                    emulator.tags_of(case, &arn).await.is_empty(),
                    "case {case:?}: an authorized removal must persist"
                );
            }
        }
    }
}
