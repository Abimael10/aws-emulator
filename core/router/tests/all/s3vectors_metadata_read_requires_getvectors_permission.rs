// SPDX-License-Identifier: CC0-1.0

//! Which permission an `s3vectors` READ stands on, and which documents have to grant it,
//! driven end to end over the three services that decide it: IAM holds the role and its
//! inline document, STS mints the session and records what it was scoped to, and
//! `s3vectors` resolves the credential and decides the call.
//!
//! THE RULE: a read whose REQUEST PARAMETERS ask for vector data or vector metadata — a
//! query asking for metadata or filtering on it, a listing asking for data or metadata —
//! stands on `s3vectors:GetVectors` BESIDES the operation's own action, so a caller holding
//! only the operation's own action is served the keys-only form of the call and refused the
//! metadata-bearing form with HTTP 403 `AccessDeniedException`; and the permissions a
//! request signed with a session credential has are the INTERSECTION of the role's
//! identity-based documents and the session policies the mint passed, so either half
//! withholding `s3vectors:GetVectors` is enough to refuse it.
//!
//! The rows span what the rule generalises over: which operation is called (`QueryVectors`,
//! `ListVectors`), which parameter form asks for the protected bytes (`returnMetadata`, a
//! metadata filter, both together, `returnData`), the forms that ask for none of it
//! (omitted, explicitly `false`, and distances only), and which half of the intersection
//! withholds the permission (the session policy, the role's own document).
//!
//! Three crates have to agree for any of it to hold, so this is the suite that can pin it:
//! no single crate's own test target can mint the session through a real `sts:AssumeRole`
//! and then read the answer the deciding service gives it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder every awsQuery suite of this crate shares, and the reader that pulls
// one element's text out of an XML answer: written ONCE in `query_wire`.
use crate::query_wire::{element, form_encode};

/// The region every request here is signed for and every ARN below carries.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// fixture is built with it and the role is assumed with it. No IAM document bounds it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The vector bucket and the index in it every row is driven against.
const BUCKET: &str = "qv-metadata-gate-bucket";
const INDEX: &str = "qv-metadata-gate-index";

/// The role the sessions are sessions of, and the name of its inline document.
const ROLE: &str = "qv-metadata-gate-role";
const ROLE_POLICY_NAME: &str = "inline";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The two vectors the index holds: the key, the data and the metadata each is written
/// with. The second one's `tenant` is what the metadata filter below selects on, so a
/// filtered query reports one of the two.
fn stored_vectors() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "alpha",
            json!({ "float32": [1.0, 0.0, 0.0] }),
            json!({ "tenant": "tenant-a", "label": "alpha" }),
        ),
        (
            "beta",
            json!({ "float32": [0.0, 1.0, 0.0] }),
            json!({ "tenant": "tenant-b", "label": "beta" }),
        ),
    ]
}

/// The metadata filter every filtering row carries: implicit equality on one key, which
/// selects the first stored vector.
fn tenant_filter() -> Value {
    json!({ "tenant": "tenant-a" })
}

/// The three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
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
    router.register_service(service_com_amazonaws_s3vectors_s3vectors::EmulatedService::new(
        engine,
    ));
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

/// One `s3vectors` call, signed with `access_key_id`, reported as `(status, body)`.
async fn s3vectors(
    router: &MultiServiceRouter,
    access_key_id: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("s3vectors.{REGION}.amazonaws.com");
    let payload = bytes::Bytes::from(serde_json::to_vec(body).expect("the body serializes"));
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization(access_key_id, "s3vectors"))
        .body(payload)
        .expect("a well-formed restJson1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
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
/// IAM's rules refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The ARN the account holds the role under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// The ARN of the index every row addresses, which is the resource a refusal names.
fn index_arn() -> String {
    format!(
        "arn:aws:s3vectors:{REGION}:{}:bucket/{BUCKET}/index/{INDEX}",
        ::engine::emulator_account_id()
    )
}

/// A policy allowing `actions` on every resource.
fn allow(actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
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

/// The credential one session was minted under: the access key id a later request signs
/// with, and the ARN `sts:AssumeRole` reports for it — the principal a refusal blames.
#[derive(Debug, Clone)]
struct Session {
    access_key_id: String,
    arn: String,
}

/// A router holding the role under `role_policy`, one vector bucket, one index in it and
/// the two vectors WITH metadata — every setup call made over the real wire with the
/// account's own credential, which no document bounds.
async fn fixture(role_policy: &Value) -> MultiServiceRouter {
    let router = build_router();

    let (status, created) = query(
        &router,
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

    let (status, attached) = query(
        &router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
            form_encode(&role_policy.to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutRolePolicy({ROLE:?}): expected 200, got {status} {attached}"
    );

    let vectors: Vec<Value> = stored_vectors()
        .into_iter()
        .map(|(key, data, metadata)| json!({ "key": key, "data": data, "metadata": metadata }))
        .collect();
    for (path, body) in [
        ("/CreateVectorBucket", json!({ "vectorBucketName": BUCKET })),
        (
            "/CreateIndex",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "dataType": "float32",
                "dimension": 3,
                "distanceMetric": "cosine",
            }),
        ),
        (
            "/PutVectors",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "vectors": vectors,
            }),
        ),
    ] {
        let (status, answered) = s3vectors(&router, AMBIENT_KEY, path, &body).await;
        assert_eq!(
            status, 200,
            "setup {path}: expected 200, got {status} {answered}"
        );
    }
    router
}

/// Mint a session of the role through a real `sts:AssumeRole` call, carrying
/// `session_policy` as the scope the credential is minted with.
async fn assume_role(
    router: &MultiServiceRouter,
    session_name: &str,
    session_policy: &Value,
) -> Session {
    let (status, body) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}&RoleSessionName={session_name}\
             &Policy={}",
            form_encode(&role_arn()),
            form_encode(&session_policy.to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup AssumeRole({session_name:?}): expected 200, got {status} {body}"
    );
    Session {
        access_key_id: element(&body, "AccessKeyId"),
        arn: element(&body, "Arn"),
    }
}

/// One request form: the call it makes, and whether its PARAMETERS make it stand on
/// `s3vectors:GetVectors` besides the operation's own action.
#[derive(Debug, Clone)]
struct Form {
    case: &'static str,
    path: &'static str,
    body: Value,
    needs_get_vectors: bool,
    /// The keys this form reports when it is served, in the order it reports them.
    expected_keys: Vec<&'static str>,
}

/// Every read form, one per way a request asks — or does not ask — for the bytes behind the
/// keys: the two `ListVectors` flags, `QueryVectors`' metadata flag, its metadata filter and
/// both together, against the forms that ask for neither (omitted, explicitly `false`, and
/// distances only).
fn read_forms() -> Vec<Form> {
    let index = json!({ "vectorBucketName": BUCKET, "indexName": INDEX });
    let query_base = json!({
        "vectorBucketName": BUCKET,
        "indexName": INDEX,
        "topK": 2,
        "queryVector": { "float32": [1.0, 0.0, 0.0] },
    });
    // (case, path, the members added to that operation's base request, GetVectors, keys)
    let rows: Vec<(&str, &str, Value, bool, Vec<&str>)> = vec![
        (
            "QueryVectors asking for neither",
            "/QueryVectors",
            json!({}),
            false,
            vec!["alpha", "beta"],
        ),
        (
            "QueryVectors with returnMetadata explicitly false",
            "/QueryVectors",
            json!({ "returnMetadata": false }),
            false,
            vec!["alpha", "beta"],
        ),
        (
            "QueryVectors asking for distances only",
            "/QueryVectors",
            json!({ "returnDistance": true }),
            false,
            vec!["alpha", "beta"],
        ),
        (
            "QueryVectors asking for metadata",
            "/QueryVectors",
            json!({ "returnMetadata": true }),
            true,
            vec!["alpha", "beta"],
        ),
        (
            "QueryVectors filtering on metadata",
            "/QueryVectors",
            json!({ "filter": tenant_filter() }),
            true,
            vec!["alpha"],
        ),
        (
            "QueryVectors filtering and asking for metadata",
            "/QueryVectors",
            json!({ "filter": tenant_filter(), "returnMetadata": true }),
            true,
            vec!["alpha"],
        ),
        (
            "ListVectors asking for neither",
            "/ListVectors",
            json!({}),
            false,
            vec!["alpha", "beta"],
        ),
        (
            "ListVectors asking for metadata",
            "/ListVectors",
            json!({ "returnMetadata": true }),
            true,
            vec!["alpha", "beta"],
        ),
        (
            "ListVectors asking for vector data",
            "/ListVectors",
            json!({ "returnData": true }),
            true,
            vec!["alpha", "beta"],
        ),
    ];
    rows.into_iter()
        .map(|(case, path, added, needs_get_vectors, expected_keys)| {
            let mut body = match path {
                "/ListVectors" => index.clone(),
                _ => query_base.clone(),
            };
            for (member, value) in added.as_object().expect("the added members are a map") {
                body[member] = value.clone();
            }
            Form {
                case,
                path,
                body,
                needs_get_vectors,
                expected_keys,
            }
        })
        .collect()
}

/// The vectors an answer reports, in the order it reports them.
fn reported(body: &Value) -> Vec<Value> {
    body.get("vectors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The keys an answer reports, in the order it reports them.
fn reported_keys(body: &Value) -> Vec<String> {
    reported(body)
        .iter()
        .map(|vector| {
            vector
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The metadata each stored vector was written with, by key.
fn written_metadata(key: &str) -> Value {
    stored_vectors()
        .into_iter()
        .find(|(stored, _, _)| *stored == key)
        .map(|(_, _, metadata)| metadata)
        .unwrap_or_else(|| panic!("no vector named {key:?} is written by this fixture"))
}

/// The data each stored vector was written with, by key.
fn written_data(key: &str) -> Value {
    stored_vectors()
        .into_iter()
        .find(|(stored, _, _)| *stored == key)
        .map(|(_, data, _)| data)
        .unwrap_or_else(|| panic!("no vector named {key:?} is written by this fixture"))
}

/// The session policy allowing only the two read operations' own actions: the narrow
/// caller, which may search and list but never read the bytes behind the keys.
fn keys_only_actions() -> Value {
    allow(json!(["s3vectors:QueryVectors", "s3vectors:ListVectors"]))
}

/// The same, with `s3vectors:GetVectors` added: the wide caller.
fn metadata_bearing_actions() -> Value {
    allow(json!([
        "s3vectors:QueryVectors",
        "s3vectors:ListVectors",
        "s3vectors:GetVectors",
    ]))
}

/// A session whose SESSION POLICY withholds `s3vectors:GetVectors` is served the keys-only
/// form of each read and refused the metadata-bearing form with the modelled 403, even
/// though the role's own document allows everything this service does — the session policy
/// half of the intersection.
#[tokio::test]
async fn a_session_policy_withholding_getvectors_refuses_only_the_metadata_bearing_reads() {
    let router = fixture(&allow(json!("s3vectors:*"))).await;
    let session = assume_role(&router, "narrow", &keys_only_actions()).await;

    for form in read_forms() {
        let case = form.case;
        let (status, body) = s3vectors(&router, &session.access_key_id, form.path, &form.body).await;
        if form.needs_get_vectors {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: a read asking for the protected bytes without \
                 s3vectors:GetVectors must be refused with the modelled 403, got {status} {body}"
            );
            assert_eq!(
                error_message(&body),
                not_authorized(
                    &session.arn,
                    "s3vectors:GetVectors",
                    &index_arn(),
                    "because no session policy allows the s3vectors:GetVectors action",
                ),
                "case {case:?}: the refusal must name the session policy as the document that \
                 withheld s3vectors:GetVectors, got {body}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: a read asking for neither data nor metadata stands on the \
             operation's own action alone and must be served, got {status} {body}"
        );
        assert_eq!(
            reported_keys(&body),
            form.expected_keys,
            "case {case:?}: expected the keys {:?}, got {body}",
            form.expected_keys
        );
        for vector in reported(&body) {
            assert_eq!(
                vector.get("metadata"),
                None,
                "case {case:?}: a served keys-only read must carry no metadata, got {vector}"
            );
            assert_eq!(
                vector.get("data"),
                None,
                "case {case:?}: a served keys-only read must carry no vector data, got {vector}"
            );
        }
    }
}

/// The positive control of the same table: a session whose policy ALSO allows
/// `s3vectors:GetVectors` is served every form, and the data and metadata it reads back are
/// the ones the vectors were written with.
#[tokio::test]
async fn a_session_policy_allowing_getvectors_is_served_every_read_form() {
    let router = fixture(&allow(json!("s3vectors:*"))).await;
    let session = assume_role(&router, "wide", &metadata_bearing_actions()).await;

    for form in read_forms() {
        let case = form.case;
        let (status, body) = s3vectors(&router, &session.access_key_id, form.path, &form.body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a read both documents allow must be served, got {status} {body}"
        );
        assert_eq!(
            reported_keys(&body),
            form.expected_keys,
            "case {case:?}: expected the keys {:?}, got {body}",
            form.expected_keys
        );
        let asks_for_metadata = form.body.get("returnMetadata") == Some(&json!(true));
        let asks_for_data = form.body.get("returnData") == Some(&json!(true));
        for vector in reported(&body) {
            let key = vector
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let expected_metadata = asks_for_metadata.then(|| written_metadata(&key));
            assert_eq!(
                vector.get("metadata").cloned(),
                expected_metadata,
                "case {case:?}: the metadata reported for {key:?} must be the metadata it was \
                 written with when the request asked for it and nothing otherwise, got {vector}"
            );
            let expected_data = asks_for_data.then(|| written_data(&key));
            assert_eq!(
                vector.get("data").cloned(),
                expected_data,
                "case {case:?}: the data reported for {key:?} must be the data it was written \
                 with when the request asked for it and nothing otherwise, got {vector}"
            );
        }
    }
}

/// The other half of the intersection: a session policy allowing everything this service
/// does grants nothing the ROLE's own document withholds, so the same metadata-bearing
/// reads are refused — and the refusal names the identity-based half rather than the scope.
#[tokio::test]
async fn the_roles_own_document_withholding_getvectors_refuses_the_metadata_bearing_reads() {
    let router = fixture(&keys_only_actions()).await;
    let session = assume_role(&router, "wide-scope", &allow(json!("s3vectors:*"))).await;

    for form in read_forms() {
        let case = form.case;
        let (status, body) = s3vectors(&router, &session.access_key_id, form.path, &form.body).await;
        if form.needs_get_vectors {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: a permission the role's own document withholds must refuse the \
                 call with the modelled 403, got {status} {body}"
            );
            assert_eq!(
                error_message(&body),
                not_authorized(
                    &session.arn,
                    "s3vectors:GetVectors",
                    &index_arn(),
                    "because no identity-based policy allows the s3vectors:GetVectors action",
                ),
                "case {case:?}: the refusal must name the identity-based half, got {body}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the keys-only form stands on the operation's own action, which both \
             documents allow, and must be served, got {status} {body}"
        );
    }
}
