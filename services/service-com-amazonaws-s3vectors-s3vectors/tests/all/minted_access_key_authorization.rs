// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a request signed with a long-term access key this account MINTED,
//! driven over the real wire through the router production uses — the s3vectors call, and
//! the IAM and STS calls that create the identity it speaks for.
//!
//! THE RULE: an operation of Amazon S3 Vectors is authorized against the identity-based
//! IAM policies in force on the principal the request's access key id speaks for, on the
//! action `s3vectors:<OperationName>` and the resource the request addresses, at the instant
//! of the call: a request no statement allows is answered `AccessDeniedException` (HTTP 403)
//! with IAM's own sentence naming that principal, the action and the resource, and performs
//! nothing, while the SAME request is served once a statement allows the action — and is
//! refused again once that statement is taken away.
//!
//! What this suite adds beside `identity_authorization.rs`, which puts the same rule under
//! documents it writes into the IAM store directly: here the identity, its credential and
//! its policy are created the way a client creates them — `iam:CreateUser`,
//! `iam:CreateAccessKey`, `iam:PutUserPolicy` and `iam:DeleteUserPolicy` over the awsQuery
//! wire — so the access key id under test is the one IAM MINTED rather than a literal, and
//! the principal a refusal blames is checked against the one `sts:GetCallerIdentity` answers
//! for that same credential.
//!
//! The table spans what the rule generalises over: the operation (a vector listing, a vector
//! read, a query, a write, the index and bucket control plane and the account-level listing),
//! and with it the resource a refusal names (an index ARN, a bucket ARN, and no resource at
//! all for the account-level action).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vector_lifecycle::{
    authorization_of, call_signed_by, encode_label, error_code, expected_bucket_arn,
    expected_index_arn, keys_of, message_of,
};

/// The bucket, index and vector every row is driven against.
const BUCKET: &str = "minted-key-bucket";
const INDEX: &str = "minted-key-index";

/// The vector keys the fixture's index holds, which a served listing reports.
const SEEDED_KEYS: [&str; 2] = ["v1", "v2"];

/// The access key id the FIXTURE signs with: a key neither IAM nor STS holds a record for,
/// which is the position AWS account root credentials are in and what every other suite in
/// this crate signs with.
const ACCOUNT_KEY_ID: &str = "AKIDTESTS3VECTORS";

/// The IAM user each test creates through `iam:CreateUser`. No policy is attached to it
/// until a row attaches one.
const USER_NAME: &str = "minted-key-reader";

/// The name of the inline policy a grant is attached under.
const POLICY_NAME: &str = "inline";

/// The API versions the awsQuery services under test publish, which an awsQuery request
/// carries as its `Version` parameter.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// A router serving this service beside IAM and STS over ONE shared engine, so the user,
/// the key and the policies the tests create through those services' own operations are the
/// records this service's authorization reads.
fn router_with_identity() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3vectors_s3vectors::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// Call one s3vectors operation by the URI its `@http` binding declares, signed with
/// `access_key_id`, and report `(status, body)`.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call_signed_by(
        router,
        access_key_id,
        "POST",
        &format!("/{operation}"),
        Some(body),
    )
    .await
}

/// One awsQuery call — `Action` plus form parameters — against the service whose signing
/// name is `signing_name`, signed with `access_key_id`.
///
/// Each component is percent-encoded through this crate's single copy of that encoding
/// ([`encode_label`]), which leaves only RFC 3986's unreserved characters literal — what an
/// `application/x-www-form-urlencoded` component takes.
async fn query_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in params {
        form.push('&');
        form.push_str(&encode_label(name));
        form.push('=');
        form.push_str(&encode_label(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("authorization", authorization_of(access_key_id, signing_name))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(form))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8(response.into_body().to_vec()).expect("a UTF-8 response body");
    (status, body)
}

/// One awsQuery call against IAM, signed with the account's own published credential — the
/// identity that administers the account's IAM records.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query_call(router, "iam", IAM_VERSION, ACCOUNT_KEY_ID, action, params).await
}

/// The text of the first `<tag>` element an awsQuery response carries.
///
/// A scan rather than a document model, as the awsQuery suites that read these same
/// responses already do
/// (`services/service-com-amazonaws-pi-performanceinsightsv20180227/tests/all/pi_harness.rs`),
/// so this suite adds no dependency edge to `Cargo.lock`: every tag read here is a LEAF of a
/// response this emulator itself serialized, whose content is text.
fn xml_element(body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("<{tag}> is absent from {body}"));
    let rest = &body[start + open.len()..];
    let end = rest
        .find(&close)
        .unwrap_or_else(|| panic!("<{tag}> is never closed in {body}"));
    rest[..end].to_string()
}

/// What one row drives: an operation, the request body it takes, and the resource a refusal
/// of it names — `None` for an account-level action, which names no resource.
struct Row {
    operation: &'static str,
    body: Value,
    resource: Option<String>,
}

/// One otherwise-valid request per operation the rule is asserted over, each addressing the
/// fixture: a listing, a read, a query, a write, the index and bucket control plane, and the
/// account-level listing. Authorization is the only reason any of them could be refused.
fn rows() -> Vec<Row> {
    let index_target = json!({ "vectorBucketName": BUCKET, "indexName": INDEX });
    vec![
        Row {
            operation: "ListVectors",
            body: index_target.clone(),
            resource: Some(expected_index_arn(BUCKET, INDEX)),
        },
        Row {
            operation: "GetVectors",
            body: json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "keys": [SEEDED_KEYS[0]],
            }),
            resource: Some(expected_index_arn(BUCKET, INDEX)),
        },
        Row {
            operation: "QueryVectors",
            body: json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 1,
                "queryVector": { "float32": [1.0, 0.0, 0.0] },
            }),
            resource: Some(expected_index_arn(BUCKET, INDEX)),
        },
        Row {
            operation: "PutVectors",
            body: json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "vectors": [{ "key": "written", "data": { "float32": [0.0, 1.0, 0.0] } }],
            }),
            resource: Some(expected_index_arn(BUCKET, INDEX)),
        },
        Row {
            operation: "GetIndex",
            body: index_target,
            resource: Some(expected_index_arn(BUCKET, INDEX)),
        },
        Row {
            operation: "ListIndexes",
            body: json!({ "vectorBucketName": BUCKET }),
            resource: Some(expected_bucket_arn(BUCKET)),
        },
        Row {
            operation: "GetVectorBucket",
            body: json!({ "vectorBucketName": BUCKET }),
            resource: Some(expected_bucket_arn(BUCKET)),
        },
        Row {
            operation: "ListVectorBuckets",
            body: json!({}),
            resource: None,
        },
    ]
}

/// A router holding one bucket, one index in it and [`SEEDED_KEYS`] vectors in that index,
/// created with the account's own credential, plus an IAM user carrying NO policy at all and
/// the access key id `iam:CreateAccessKey` minted for it.
async fn fixture() -> (MultiServiceRouter, MintedUser) {
    let router = router_with_identity();
    for (operation, body) in [
        ("CreateVectorBucket", json!({ "vectorBucketName": BUCKET })),
        (
            "CreateIndex",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "dataType": "float32",
                "dimension": 3,
                "distanceMetric": "cosine",
            }),
        ),
        (
            "PutVectors",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "vectors": [
                    { "key": SEEDED_KEYS[0], "data": { "float32": [1.0, 0.0, 0.0] } },
                    { "key": SEEDED_KEYS[1], "data": { "float32": [0.0, 1.0, 0.0] } },
                ],
            }),
        ),
    ] {
        let (status, answered) = call_as(&router, ACCOUNT_KEY_ID, operation, &body).await;
        assert_eq!(
            status, 200,
            "the fixture's {operation} must succeed, got {status} {answered}"
        );
    }
    let user = minted_user(&router).await;
    (router, user)
}

/// An IAM user with no policy, and the long-term access key IAM minted for it.
struct MintedUser {
    /// The user's ARN, as `iam:CreateUser` answered it.
    arn: String,
    /// The access key id `iam:CreateAccessKey` minted, which a request signs with.
    key_id: String,
}

/// Create the user and its access key the way a client does, over IAM's own wire.
async fn minted_user(router: &MultiServiceRouter) -> MintedUser {
    let (status, created) = iam_call(router, "CreateUser", &[("UserName", USER_NAME)]).await;
    assert_eq!(
        status, 200,
        "the fixture's CreateUser must succeed, got {status} {created}"
    );
    let arn = xml_element(&created, "Arn");
    let (status, minted) = iam_call(router, "CreateAccessKey", &[("UserName", USER_NAME)]).await;
    assert_eq!(
        status, 200,
        "the fixture's CreateAccessKey must succeed, got {status} {minted}"
    );
    let key_id = xml_element(&minted, "AccessKeyId");
    assert!(
        !key_id.is_empty() && key_id != ACCOUNT_KEY_ID,
        "CreateAccessKey must mint a key of its own, got {key_id:?}"
    );
    MintedUser { arn, key_id }
}

/// Put an inline policy allowing `action` on every resource in force on the user — what
/// `iam:PutUserPolicy` does.
async fn grant(router: &MultiServiceRouter, action: &str) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
    })
    .to_string();
    let (status, body) = iam_call(
        router,
        "PutUserPolicy",
        &[
            ("UserName", USER_NAME),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutUserPolicy granting {action} must succeed, got {status} {body}"
    );
}

/// Take that inline policy off the user again — what `iam:DeleteUserPolicy` does.
async fn revoke(router: &MultiServiceRouter) {
    let (status, body) = iam_call(
        router,
        "DeleteUserPolicy",
        &[("UserName", USER_NAME), ("PolicyName", POLICY_NAME)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteUserPolicy must succeed, got {status} {body}"
    );
}

/// IAM's own sentence for a request no statement allows, built from the principal, the
/// action and the resource the request named.
fn default_deny(principal: &str, action: &str, resource: Option<&str>) -> String {
    match resource {
        Some(resource) => format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} \
             because no identity-based policy allows the {action} action"
        ),
        None => format!(
            "User: {principal} is not authorized to perform: {action} because no identity-based \
             policy allows the {action} action"
        ),
    }
}

/// Assert that `(status, body)` is the modelled 403 refusal of `action` for `principal`.
fn assert_default_deny(
    case: &str,
    status: u16,
    body: &Value,
    principal: &str,
    action: &str,
    resource: Option<&str>,
) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: expected the modelled 403 AccessDeniedException, got {status} {body}"
    );
    let message = message_of(body);
    assert_eq!(
        message,
        default_deny(principal, action, resource),
        "case {case:?}: expected IAM's refusal sentence, got {message:?}"
    );
}

/// Every operation, under a key IAM minted for a user carrying no policy: the call is
/// refused with IAM's sentence, and the SAME call is served once a statement allowing its
/// action is attached to that same user.
#[tokio::test]
async fn a_minted_key_is_refused_until_a_policy_allows_the_action() {
    for row in rows() {
        let action = format!("s3vectors:{}", row.operation);
        // A fresh fixture per row, so a row's mutating call cannot change what the next one
        // is answered.
        let (router, user) = fixture().await;

        let (status, body) = call_as(&router, &user.key_id, row.operation, &row.body).await;
        assert_default_deny(
            row.operation,
            status,
            &body,
            &user.arn,
            &action,
            row.resource.as_deref(),
        );

        grant(&router, &action).await;
        let (status, body) = call_as(&router, &user.key_id, row.operation, &row.body).await;
        assert_eq!(
            status, 200,
            "case {:?}: the granted call must be served, got {status} {body}",
            row.operation
        );
    }
}

/// A served `ListVectors` reports the index's OWN keys — the grant makes the listing
/// observable, and what it answers comes from the vectors the fixture stored.
#[tokio::test]
async fn a_granted_listing_reports_the_indexs_stored_keys() {
    let (router, user) = fixture().await;
    grant(&router, "s3vectors:ListVectors").await;

    let (status, body) = call_as(
        &router,
        &user.key_id,
        "ListVectors",
        &json!({ "vectorBucketName": BUCKET, "indexName": INDEX }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the granted ListVectors must be served, got {status} {body}"
    );
    let mut keys = keys_of(&body);
    keys.sort();
    assert_eq!(
        keys,
        SEEDED_KEYS.map(str::to_string).to_vec(),
        "the listing must report the index's stored keys, got {body}"
    );
}

/// The decision is taken from the documents in force AT THE INSTANT of the call: the same
/// request is refused, then served once a grant is attached, then refused again once that
/// grant is taken away — and every refusal names the principal `sts:GetCallerIdentity`
/// answers for the credential the request signed with.
#[tokio::test]
async fn a_grant_takes_effect_on_the_next_call_and_lifts_when_it_is_removed() {
    let (router, user) = fixture().await;
    let action = "s3vectors:ListVectors";
    let body = json!({ "vectorBucketName": BUCKET, "indexName": INDEX });
    let resource = expected_index_arn(BUCKET, INDEX);

    // Who that credential speaks for, read from the service that answers it, so the
    // principal a refusal blames is checked against STS's own answer rather than a literal.
    let (status, identity) = query_call(
        &router,
        "sts",
        STS_VERSION,
        &user.key_id,
        "GetCallerIdentity",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "GetCallerIdentity for the minted key must succeed, got {status} {identity}"
    );
    let reported = xml_element(&identity, "Arn");
    assert_eq!(
        reported, user.arn,
        "the minted key must speak for the user CreateUser answered, got {reported:?}"
    );

    for (case, granted) in [
        ("no policy at all", false),
        ("the action granted", true),
        ("the grant removed", false),
    ] {
        match granted {
            true => grant(&router, action).await,
            // The first row runs before any grant is attached, where there is nothing to
            // remove; the last runs after one was, which is what it takes away.
            false if case == "the grant removed" => revoke(&router).await,
            false => {}
        }
        let (status, answered) = call_as(&router, &user.key_id, "ListVectors", &body).await;
        match granted {
            true => assert_eq!(
                status, 200,
                "case {case:?}: the granted call must be served, got {status} {answered}"
            ),
            false => assert_default_deny(
                case,
                status,
                &answered,
                &user.arn,
                action,
                Some(&resource),
            ),
        }
    }
}

/// A refused write performs nothing: the index still holds exactly the vectors the fixture
/// stored, as the account's own credential reads them back.
#[tokio::test]
async fn a_refused_call_performs_nothing() {
    let (router, user) = fixture().await;

    let (status, body) = call_as(
        &router,
        &user.key_id,
        "PutVectors",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": INDEX,
            "vectors": [{ "key": "unwritten", "data": { "float32": [0.0, 0.0, 1.0] } }],
        }),
    )
    .await;
    assert_default_deny(
        "a write no policy allows",
        status,
        &body,
        &user.arn,
        "s3vectors:PutVectors",
        Some(&expected_index_arn(BUCKET, INDEX)),
    );

    let (status, listed) = call_as(
        &router,
        ACCOUNT_KEY_ID,
        "ListVectors",
        &json!({ "vectorBucketName": BUCKET, "indexName": INDEX }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the account's own listing must be served, got {status} {listed}"
    );
    let mut keys = keys_of(&listed);
    keys.sort();
    assert_eq!(
        keys,
        SEEDED_KEYS.map(str::to_string).to_vec(),
        "the refused write must have stored nothing, got {listed}"
    );
}
