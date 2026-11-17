// SPDX-License-Identifier: CC0-1.0

//! What a supply chain tag write persists, and who may make it — driven end to end over the
//! three services that decide it: IAM holds the role and the inline document in force on it,
//! STS mints the session the call is signed with, and the supply chain service resolves that
//! credential, decides the call and stores the tags.
//!
//! RULES under test:
//!
//! * a tag write MERGES the keys it carries into the tags the addressed resource already
//!   holds — adding a key that is new and replacing the value of one that is not — and a
//!   resource never comes to hold more tags than the model admits in ONE write (`TagMap`'s
//!   own `@length` max), so a write that would push the STORED count past that bound is
//!   refused `ValidationException` (HTTP 400) and persists nothing, while a write that lands
//!   exactly on it, or only replaces a value, is served; the bound is per resource, whatever
//!   kind of resource the ARN names;
//! * the call is decided against the identity-based policy documents in force on the
//!   principal the request's credential speaks for, on the resource the request ADDRESSED,
//!   before anything is read or written: an action no readable document allows, or one an
//!   explicit `Deny` covers, is answered HTTP 403 `AccessDeniedException` and no tag is
//!   written.
//!
//! Only a cross-service suite can pin the second half as a client meets it: the session has
//! to be minted by a real `sts:AssumeRole` of a role a real `iam:CreateRole` holds, under a
//! document a real `iam:PutRolePolicy` stored, and then read back through the answer the
//! supply chain service gives a request signed with it.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder this test target already shares for a URI path segment carrying an ARN,
// the form-encoder every awsQuery suite of this crate shares, and the reader that pulls one
// element's text out of an XML answer: each written ONCE, in the sibling that owns it.
use crate::cloudfront_key_value_store_handshake::segment;
use crate::query_wire::{element, form_encode};

/// The region every request here is signed for and every ARN below carries.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// fixture is built with it and the role is assumed with it. No IAM document bounds it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The IAM action namespace this service publishes its operations under, which is also its
/// SigV4 signing name and the `arnNamespace` its own model declares
/// (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`).
const SCN: &str = "scn";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The role the session is a session of, and the name of its inline document.
const ROLE: &str = "scn-tag-writer";
const ROLE_POLICY_NAME: &str = "inline";

/// How many tags one resource of this service can come to hold: the max of `TagMap`'s own
/// `@length`
/// (`smithy_query(query="shape", shape="TagMap", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`
/// reads `{"min": 0, "max": 200}`), which is what the model admits in one write — so no
/// sequence of merging writes may leave a resource holding more than a single call could have
/// named.
const MAX_TAGS: usize = 200;

/// The key a row writes once the resource is full: the one write that must be refused.
const OVERFLOW_KEY: &str = "one-too-many";

/// The sentence this service refuses a tag write past that bound with, `bound` being the bound
/// it was refused against. Written out HERE rather than read from the crate: the wording is the
/// requirement, so a drift in what the handler answers fails this suite instead of travelling
/// with it.
fn tag_bound_reached_message(bound: usize) -> String {
    format!("the number of tags on this resource has reached the quota of {bound}")
}

/// The three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise operation behaviour, not request signing.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key a service reads the caller's identity from. The
/// signature is never checked: request authentication is off on this router.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One awsQuery form call to `signing_name`, signed with the account's own credential,
/// reported as `(status, the XML text the wire carried)`.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One supply chain restJson1 call, signed with `access_key_id`, reported as
/// `(status, body)`.
async fn scn(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let host = format!("{SCN}.{REGION}.amazonaws.com");
    let payload = match body {
        Some(value) => {
            bytes::Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        }
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization(access_key_id, SCN))
        .body(payload)
        .expect("a well-formed restJson1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let parsed = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of an error response, as the short shape name a client reads.
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// The error body's `message`, under either casing the envelope may carry it in.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// IAM's refusal sentence: who was refused, which action, on which resource, and which of
/// IAM's rules refused it. Written out HERE rather than read from a crate: the wording is the
/// requirement, so a drift in what the services answer fails this suite instead of travelling
/// with it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause for a request no readable identity-based document allows — IAM's default
/// deny, which names the action nothing granted.
fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause for a request an explicit `Deny` in one of the principal's own
/// identity-based documents refuses.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The ARN the account holds the role under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// The ARN of one instance of this service, as the service mints it.
fn instance_arn(instance_id: &str) -> String {
    format!(
        "arn:aws:{SCN}:{REGION}:{}:instance/{instance_id}",
        ::engine::emulator_account_id()
    )
}

/// A policy allowing `actions` on `resource`.
fn allow(actions: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }],
    })
}

/// A role trust policy admitting this account's own principals to `sts:AssumeRole`.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// A live instance of this account, created through `CreateInstance`.
async fn given_instance(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = scn(
        router,
        AMBIENT_KEY,
        "POST",
        "/api/instance",
        Some(&json!({ "instanceName": name })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateInstance({name:?}): expected 200, got {status} {body}"
    );
    body["instance"]["instanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateInstance({name:?}) carried no instanceId: {body}"))
        .to_string()
}

/// A data lake namespace of `instance_id`, created through `CreateDataLakeNamespace`.
async fn given_namespace(router: &MultiServiceRouter, instance_id: &str, name: &str) {
    let (status, body) = scn(
        router,
        AMBIENT_KEY,
        "PUT",
        &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateDataLakeNamespace({name:?}): expected 200, got {status} {body}"
    );
}

/// A data lake dataset of `instance_id`'s `namespace`, created through
/// `CreateDataLakeDataset`.
async fn given_dataset(
    router: &MultiServiceRouter,
    instance_id: &str,
    namespace: &str,
    name: &str,
) {
    let (status, body) = scn(
        router,
        AMBIENT_KEY,
        "PUT",
        &format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}"),
        Some(&json!({
            "schema": {
                "name": "Shipment",
                "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateDataLakeDataset({name:?}): expected 200, got {status} {body}"
    );
}

/// One `TagResource` call for `resource_arn`, signed with `access_key_id`.
async fn tag(
    router: &MultiServiceRouter,
    access_key_id: &str,
    resource_arn: &str,
    tags: &Value,
) -> (u16, Value) {
    scn(
        router,
        access_key_id,
        "POST",
        &format!("/api/tags/{}", segment(resource_arn)),
        Some(&json!({ "tags": tags })),
    )
    .await
}

/// The tags `ListTagsForResource` reports for `resource_arn`, read with the account's own
/// credential — the reading of the stored record every case checks its write against.
async fn tags_of(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
) -> BTreeMap<String, String> {
    let (status, body) = scn(
        router,
        AMBIENT_KEY,
        "GET",
        &format!("/api/tags/{}", segment(resource_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTagsForResource expected 200, got {status} {body}"
    );
    let reported = body
        .get("tags")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no `tags` map: {body}"));
    reported
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the value of tag {key:?} is not a string: {body}")
                    })
                    .to_string(),
            )
        })
        .collect()
}

/// The key of the `index`-th tag a fill writes.
fn tag_key(index: usize) -> String {
    format!("key-{index:03}")
}

/// The value that tag is written with.
fn tag_value(index: usize) -> String {
    format!("value-{index:03}")
}

/// A tag map naming `count` keys from `first`, each with its own value.
fn tag_map(first: usize, count: usize) -> Value {
    let mut map = serde_json::Map::new();
    for index in first..first + count {
        map.insert(tag_key(index), Value::String(tag_value(index)));
    }
    Value::Object(map)
}

/// The tags a resource holds once a row's fills have landed: the keys `0..MAX_TAGS` with the
/// values they were written with, whichever way the fills were split.
fn filled_tags() -> BTreeMap<String, String> {
    (0..MAX_TAGS).map(|i| (tag_key(i), tag_value(i))).collect()
}

/// One row of the bound's table: the resource it addresses, and the writes that fill it to
/// the bound.
struct Fill {
    case: &'static str,
    arn: String,
    writes: Vec<Value>,
}

#[tokio::test]
async fn a_resource_holds_no_more_tags_than_one_write_may_name() {
    let router = build_router();
    let instance_id = given_instance(&router, "tag bound").await;
    given_namespace(&router, &instance_id, "tag_bound").await;
    given_dataset(&router, &instance_id, "tag_bound", "shipments").await;

    // The rows differ in the kind of resource the ARN names, and in how the fill is SPLIT —
    // one write landing exactly on the bound, two disjoint writes, and two that overlap — so
    // what is checked is the count the resource STORES rather than the size of a request.
    let rows = [
        Fill {
            case: "an instance filled by one write landing on the bound",
            arn: instance_arn(&instance_id),
            writes: vec![tag_map(0, MAX_TAGS)],
        },
        Fill {
            case: "a namespace filled by two overlapping writes",
            arn: format!("{}/namespaces/tag_bound", instance_arn(&instance_id)),
            writes: vec![tag_map(0, 150), tag_map(140, 60)],
        },
        Fill {
            case: "a dataset filled one key short and then topped up",
            arn: format!(
                "{}/namespaces/tag_bound/datasets/shipments",
                instance_arn(&instance_id)
            ),
            writes: vec![tag_map(0, MAX_TAGS - 1), tag_map(MAX_TAGS - 1, 1)],
        },
    ];

    for row in rows {
        let case = row.case;
        assert!(
            tags_of(&router, case, &row.arn).await.is_empty(),
            "case {case:?}: a resource created with no tags must report none"
        );

        for (ordinal, write) in row.writes.iter().enumerate() {
            let (status, body) = tag(&router, AMBIENT_KEY, &row.arn, write).await;
            assert_eq!(
                status, 200,
                "case {case:?}: write {ordinal} up to the bound expected 200, got {status} {body}"
            );
        }
        assert_eq!(
            tags_of(&router, case, &row.arn).await,
            filled_tags(),
            "case {case:?}: the read must report exactly what the merging writes persisted"
        );

        // One key past the bound: refused, and every tag the resource carried is left alone.
        let (status, body) = tag(
            &router,
            AMBIENT_KEY,
            &row.arn,
            &json!({ OVERFLOW_KEY: "later" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a write past the bound must be refused with the modelled 400, got \
             {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            tag_bound_reached_message(MAX_TAGS),
            "case {case:?}: the refusal must state the bound it refused against, got {body}"
        );
        assert_eq!(
            tags_of(&router, case, &row.arn).await,
            filled_tags(),
            "case {case:?}: a refused write must persist nothing"
        );

        // A write that only REPLACES a value grows nothing, so it is served at the bound.
        let (status, body) = tag(
            &router,
            AMBIENT_KEY,
            &row.arn,
            &json!({ tag_key(0): "replaced" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: replacing a value at the bound expected 200, got {status} {body}"
        );
        let mut expected = filled_tags();
        expected.insert(tag_key(0), "replaced".to_string());
        assert_eq!(
            tags_of(&router, case, &row.arn).await,
            expected,
            "case {case:?}: a replacing write must change that key's value and no other tag"
        );

        // A write naming no key at all adds nothing and replaces nothing.
        let (status, body) = tag(&router, AMBIENT_KEY, &row.arn, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an empty write at the bound expected 200, got {status} {body}"
        );
        assert_eq!(
            tags_of(&router, case, &row.arn).await,
            expected,
            "case {case:?}: an empty write must leave every stored tag alone"
        );
    }
}

/// The credential one session was minted under: the access key id a later request signs with,
/// and the ARN `sts:AssumeRole` reports for it — the principal a refusal blames.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Hold the role, through IAM's own `CreateRole`, and mint a session of it through a real
/// `sts:AssumeRole`.
async fn given_session(router: &MultiServiceRouter) -> Session {
    let (status, created) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust_policy().to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({ROLE:?}): expected 200, got {status} {created}"
    );
    assert_eq!(
        element(&created, "Arn"),
        role_arn(),
        "setup CreateRole({ROLE:?}): the role must be held under its own ARN, got {created}"
    );

    let (status, assumed) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}&RoleSessionName=tagging",
            form_encode(&role_arn())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup AssumeRole({ROLE:?}): expected 200, got {status} {assumed}"
    );
    Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// Put `document` in force on the role as its inline policy, replacing whatever it carried,
/// through IAM's own `PutRolePolicy`.
async fn given_role_policy(router: &MultiServiceRouter, document: &Value) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
            form_encode(&document.to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutRolePolicy({ROLE:?}): expected 200, got {status} {body}"
    );
}

#[tokio::test]
async fn a_tag_write_is_decided_against_the_documents_in_force_on_the_session() {
    let router = build_router();
    let instance_id = given_instance(&router, "tag authorization").await;
    let arn = instance_arn(&instance_id);
    let session = given_session(&router).await;
    let action = format!("{SCN}:TagResource");
    // An instance ARN of the same account that is NOT the one the request addresses, for the
    // row whose grant is scoped to another resource.
    let other_arn = instance_arn("00000000-0000-4000-8000-000000000000");

    // The rows differ in what the document in force says about this action on this resource:
    // a grant covering every action, a grant naming exactly this action and resource, a grant
    // of a different action, a grant of this action on a different resource, and a broad grant
    // an explicit `Deny` overrides. `None` is the refusal a served row expects to meet.
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "every action of this service on every resource",
            allow(json!(format!("{SCN}:*")), json!("*")),
            None,
        ),
        (
            "this action on exactly this resource",
            allow(json!(action.clone()), json!(arn.clone())),
            None,
        ),
        (
            "another action of this service only",
            allow(json!(format!("{SCN}:ListTagsForResource")), json!("*")),
            Some(no_policy_allows(&action)),
        ),
        (
            "every action of this service on another resource",
            allow(json!(format!("{SCN}:*")), json!(other_arn.clone())),
            Some(no_policy_allows(&action)),
        ),
        (
            "a broad grant an explicit deny overrides",
            json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": format!("{SCN}:*"), "Resource": "*" },
                    { "Effect": "Deny", "Action": action.clone(), "Resource": "*" },
                ],
            }),
            Some(EXPLICIT_DENY.to_string()),
        ),
    ];

    for (ordinal, (case, document, refusal)) in rows.into_iter().enumerate() {
        given_role_policy(&router, &document).await;
        let key = format!("row-{ordinal}");
        let (status, body) = tag(
            &router,
            &session.access_key_id,
            &arn,
            &json!({ key.clone(): "written" }),
        )
        .await;

        match refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a write a matching Allow covers expected 200, got {status} \
                     {body}"
                );
                assert_eq!(
                    tags_of(&router, case, &arn).await.get(&key).cloned(),
                    Some("written".to_string()),
                    "case {case:?}: a served write must persist the tag it carried"
                );
            }
            Some(reason) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (403, "AccessDeniedException"),
                    "case {case:?}: a write no document allows must be refused with the modelled \
                     403, got {status} {body}"
                );
                assert_eq!(
                    error_message(&body),
                    not_authorized(&session.arn, &action, &arn, &reason),
                    "case {case:?}: the refusal must name the principal STS minted, the action \
                     asked for, the resource addressed and the rule that refused, got {body}"
                );
                assert!(
                    !tags_of(&router, case, &arn).await.contains_key(&key),
                    "case {case:?}: a refused write must persist nothing"
                );
            }
        }
    }
}
