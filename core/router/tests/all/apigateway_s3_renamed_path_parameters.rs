// SPDX-License-Identifier: CC0-1.0

//! An API Gateway method whose `authorizationType` is `NONE`, served by an `AWS` integration
//! to Amazon S3 whose `requestParameters` RENAME the path parameters the call is addressed by.
//!
//! RULE, in two halves, each one level more general than the method below:
//!
//! * `Method$authorizationType` `NONE` is "open access"
//!   (`smithy_query(query="shape",shape="Method",member="authorizationType")`), so API
//!   Gateway authenticates no principal for such a method and takes NO `execute-api:Invoke`
//!   identity decision about it: who the request speaks for is decided by the METHOD rather
//!   than by what the request carries, and the integration runs whether the request presents
//!   a credential or none at all;
//! * every `{name}` placeholder of an `AWS` integration's `uri` takes the value the
//!   integration maps onto the integration request's path parameter of that name
//!   (`integration.request.path.{name}` in `Integration$requestParameters`), and the SOURCE
//!   of that value is whatever method request parameter the mapping names — a path, query or
//!   header parameter, or a static literal. The two names are independent: a mapping is read
//!   by what it names, never by the placeholder and the method parameter happening to be
//!   spelled alike, so the bucket and the key of an Amazon S3 `path` form are each addressed
//!   at the value its own source carries for this request.
//!
//! So an unsigned `GET` of a `NONE` method on `/{folder}/{item}`, integrated as
//! `path/{bucket}/{key}` with `bucket` mapped from `method.request.path.folder` and `key`
//! from `method.request.path.item`, is answered with the object that bucket holds under that
//! key — and never with an authorization refusal naming a permission AWS requires for no
//! such method.
//!
//! Only a cross-service suite can pin this: the objects are Amazon S3's, the integration's
//! role and its grant are IAM's, and the api, the stage and the parameter mapping are API
//! Gateway's, so the rule is observable only where all three are mounted on one engine.
//!
//! The rows vary what the rule generalises over: WHICH bucket the request names (two, holding
//! different bytes under one key, so the bucket really comes from the request), whether the
//! placeholder and the method parameter are spelled alike or renamed, the SOURCE the value is
//! mapped from (a path parameter, a greedy path parameter, a header, a query parameter), the
//! ACTION (read, head, write, delete), and the status the service answers (an object that is
//! there, a key nothing is stored under, a bucket the role's grant does not cover). The
//! credential the request presents varies too — none, and one naming a principal nothing
//! holds — because the method's own authorization is what decides.
//!
//! The harness is the sibling suite's, which owns this rule's other half
//! ([`super::apigateway_s3_service_integration`]): one emulator, one reading of each service's
//! wire form, so the two suites cannot drift on how a stage, a role or an object is made.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_s3_service_integration::{
    apigw_ok, emulator, invoke, member, policy, resource, role, s3, Answer, APIGATEWAY_TRUST,
    MAPPED_STATUSES, REGION, STAGE,
};

/// The bucket the renaming rows address first.
const BUCKET_ONE: &str = "renamed-shelf-one";

/// A SECOND bucket, holding different bytes under the same key: the row that addresses it
/// differs from the row above only in the value the request carries for the bucket
/// parameter.
const BUCKET_TWO: &str = "renamed-shelf-two";

/// A bucket the integration's role is granted nothing on, which exists all the same — so the
/// refusal its row is answered with is about the grant rather than about the bucket being
/// absent.
const LOCKED_BUCKET: &str = "locked-shelf";

/// The key both shelves hold, under different bytes.
const ALPHA_KEY: &str = "alpha.txt";

/// A key under a PREFIX, which a greedy path parameter carries whole.
const NESTED_KEY: &str = "notes/today.txt";

/// A key both shelves hold, which the delete row removes from one of them.
const SPARE_KEY: &str = "spare.txt";

/// The key the write row stores under.
const WRITTEN_KEY: &str = "written.txt";

/// A key no shelf holds anything under.
const ABSENT_KEY: &str = "absent.txt";

/// The bytes each stored object carries, distinct per bucket so a row's answer names which
/// bucket served it.
const ONE_ALPHA: &[u8] = b"shelf one: alpha, as stored";
const TWO_ALPHA: &[u8] = b"shelf two: alpha, as stored";
const ONE_NESTED: &[u8] = b"shelf one: the note under a prefix";
const SPARE_BYTES: &[u8] = b"a spare object";
const LOCKED_TEXT: &str = "behind the grant";
const WRITTEN_BYTES: &[u8] = b"written through the renamed parameters";

/// The media type every text object here is stored under.
const TEXT_MEDIA_TYPE: &str = "text/plain";

/// The header the header-sourced mapping reads the bucket from.
const SHELF_HEADER: &str = "x-shelf";

/// The query parameter the query-sourced mapping reads the bucket from.
const SHELF_QUERY: &str = "shelf";

/// The sentence an `execute-api:Invoke` authorization failure is worded with, which NO row
/// here may be answered by: the method authorizes nobody, so no such decision is taken at
/// all.
const INVOKE_AUTHORIZATION: &str = "execute-api:Invoke";

/// The principal such a refusal names a caller that authenticated nobody by.
const ANONYMOUS: &str = "anonymous";

/// The access key id of a credential no store holds, for the row that presents one: a `NONE`
/// method authenticates nobody, so it is served exactly as the unsigned rows are. (This
/// harness runs the front door with request authentication off, as every suite here does, so
/// what the row exercises is the STAGE's reading of the method's authorization.)
const UNHELD_KEY: &str = "AKIAUNHELDEXAMPLEKEY";

/// One row: a request to the stage and what the records say it is answered with.
struct Row {
    case: &'static str,
    verb: &'static str,
    path: String,
    headers: Vec<(&'static str, &'static str)>,
    body: &'static [u8],
    expected_status: u16,
    /// The bytes the method response carries, when the rule fixes them.
    expected_body: Option<&'static [u8]>,
    /// Text the body must carry, for an answer whose envelope is the service's own.
    expected_contains: &'static [&'static str],
}

/// A SigV4 `Authorization` header naming `access_key`, scoped to the `execute-api` service
/// the invoke endpoint is published under.
fn presented_credential(access_key: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/20260101/{REGION}/execute-api/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One method of `resource_id` whose integration is the Amazon S3 call `uri` names under
/// `credentials`, with `mapped` naming, for each `{placeholder}` of that `uri`, the method
/// request parameter its value is mapped from.
///
/// The method's `authorizationType` is `NONE`, which is the half of the rule every row is
/// invoked under.
async fn renamed_method(
    router: &MultiServiceRouter,
    api: &str,
    resource_id: &str,
    verb: &str,
    uri: &str,
    credentials: &str,
    mapped: &[(&str, &str)],
) {
    let case = format!("{verb} {uri} {mapped:?}");
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/{verb}"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    let mut parameters = serde_json::Map::new();
    for (placeholder, source) in mapped {
        parameters.insert(
            format!("integration.request.path.{placeholder}"),
            json!(source),
        );
    }
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/{verb}/integration"),
        Some(json!({
            "type": "AWS",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports for it.
            "httpMethod": verb,
            "uri": uri,
            "credentials": credentials,
            "requestParameters": Value::Object(parameters),
        })),
        201,
    )
    .await;
    for (status, pattern) in MAPPED_STATUSES {
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!("/restapis/{api}/resources/{resource_id}/methods/{verb}/responses/{status}"),
            Some(json!({})),
            201,
        )
        .await;
        let mut response = json!({ "statusCode": status });
        if let Some(pattern) = pattern {
            response["selectionPattern"] = json!(pattern);
        }
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource_id}/methods/{verb}/integration/responses/{status}"
            ),
            Some(response),
            201,
        )
        .await;
    }
}

/// One bucket, with the objects `objects` names stored in it through Amazon S3's own API — so
/// what a stage serves for them is what that service holds.
async fn shelf(router: &MultiServiceRouter, bucket: &str, objects: &[(&str, &[u8])]) {
    let created = s3(router, "PUT", &format!("/{bucket}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateBucket({bucket}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    for (key, bytes) in objects {
        let written = s3(
            router,
            "PUT",
            &format!("/{bucket}/{key}"),
            &[("content-type", TEXT_MEDIA_TYPE)],
            Bytes::from(bytes.to_vec()),
        )
        .await;
        assert_eq!(
            written.status,
            200,
            "setup: PutObject({bucket}/{key}) must succeed, got {} with {:?}",
            written.status,
            written.text()
        );
    }
}

/// The stage every row below is invoked through: two shelves, a role this api may assume that
/// is granted the object actions on them, and one api whose methods map the `uri`'s bucket and
/// key placeholders from differently named method request parameters.
///
/// Answers the api's id.
async fn stage_with_renamed_parameters(router: &MultiServiceRouter) -> String {
    shelf(
        router,
        BUCKET_ONE,
        &[
            (ALPHA_KEY, ONE_ALPHA),
            (NESTED_KEY, ONE_NESTED),
            (SPARE_KEY, SPARE_BYTES),
        ],
    )
    .await;
    shelf(
        router,
        BUCKET_TWO,
        &[(ALPHA_KEY, TWO_ALPHA), (SPARE_KEY, SPARE_BYTES)],
    )
    .await;
    shelf(router, LOCKED_BUCKET, &[(ALPHA_KEY, LOCKED_TEXT.as_bytes())]).await;

    // One role API Gateway may assume, granted the object actions on the two shelves whose
    // names share a prefix — and on nothing in `LOCKED_BUCKET`.
    let reader = role(
        router,
        "renamed-parameter-reader",
        APIGATEWAY_TRUST,
        &policy(
            r#""s3:GetObject","s3:PutObject","s3:DeleteObject""#,
            "arn:aws:s3:::renamed-shelf-*/*",
        ),
    )
    .await;

    let api_body = apigw_ok(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "renamed-path-parameters" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_body, "id");
    let root = member("CreateRestApi", &api_body, "rootResourceId");

    // The `uri` every method here carries: Amazon S3's own REST path with BOTH its bucket and
    // its key left as placeholders, so each is addressed at whatever the mapping resolves for
    // this request.
    let path_form = format!("arn:aws:apigateway:{REGION}:s3:path/{{bucket}}/{{key}}");

    // The method the issue's shape describes: `/{folder}/{item}`, with the placeholders
    // mapped from parameters spelled differently from them.
    let folder = resource(router, &api, &root, "{folder}").await;
    let item = resource(router, &api, &folder, "{item}").await;
    for verb in ["GET", "HEAD", "PUT", "DELETE"] {
        renamed_method(
            router,
            &api,
            &item,
            verb,
            &path_form,
            &reader,
            &[
                ("bucket", "method.request.path.folder"),
                ("key", "method.request.path.item"),
            ],
        )
        .await;
    }

    // The same mapping where the two names AGREE, which the rule reads no differently.
    let direct = resource(router, &api, &root, "direct").await;
    let direct_bucket = resource(router, &api, &direct, "{bucket}").await;
    let direct_key = resource(router, &api, &direct_bucket, "{key}").await;
    renamed_method(
        router,
        &api,
        &direct_key,
        "GET",
        &path_form,
        &reader,
        &[
            ("bucket", "method.request.path.bucket"),
            ("key", "method.request.path.key"),
        ],
    )
    .await;

    // A GREEDY path parameter renamed onto the key, which carries a key under a prefix whole.
    let deep = resource(router, &api, &root, "deep").await;
    let deep_box = resource(router, &api, &deep, "{box}").await;
    let deep_trail = resource(router, &api, &deep_box, "{trail+}").await;
    renamed_method(
        router,
        &api,
        &deep_trail,
        "GET",
        &path_form,
        &reader,
        &[
            ("bucket", "method.request.path.box"),
            ("key", "method.request.path.trail"),
        ],
    )
    .await;

    // The bucket mapped from a HEADER and from a QUERY parameter, with the key still mapped
    // from a renamed path parameter: the source kind is the mapping's business, not the
    // placeholder's.
    let header_source = format!("method.request.header.{SHELF_HEADER}");
    let query_source = format!("method.request.querystring.{SHELF_QUERY}");
    let sourced = resource(router, &api, &root, "sourced").await;
    let by_header = resource(router, &api, &sourced, "by-header").await;
    let header_item = resource(router, &api, &by_header, "{item}").await;
    renamed_method(
        router,
        &api,
        &header_item,
        "GET",
        &path_form,
        &reader,
        &[
            ("bucket", header_source.as_str()),
            ("key", "method.request.path.item"),
        ],
    )
    .await;
    let by_query = resource(router, &api, &sourced, "by-query").await;
    let query_item = resource(router, &api, &by_query, "{item}").await;
    renamed_method(
        router,
        &api,
        &query_item,
        "GET",
        &path_form,
        &reader,
        &[
            ("bucket", query_source.as_str()),
            ("key", "method.request.path.item"),
        ],
    )
    .await;

    apigw_ok(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// Every assertion each row's answer has to satisfy, whatever it is: the status the records
/// fix, the bytes the rule fixes, and — for every row — that the answer is the stage's own
/// rather than an authorization refusal of a decision no `NONE` method takes.
fn assert_row(row: &Row, answer: &Answer) {
    assert_eq!(
        answer.status,
        row.expected_status,
        "case {:?}: expected HTTP {}, got {} with {:?}",
        row.case,
        row.expected_status,
        answer.status,
        answer.text()
    );
    if let Some(expected) = row.expected_body {
        assert_eq!(
            answer.body.as_ref(),
            expected,
            "case {:?}: expected the bytes the named bucket holds under the named key, got {:?}",
            row.case,
            answer.text()
        );
    }
    let body = answer.text();
    for expected in row.expected_contains {
        assert!(
            body.contains(expected),
            "case {:?}: expected the body to carry {expected:?}, got {body:?}",
            row.case
        );
    }
    assert!(
        !body.contains(INVOKE_AUTHORIZATION) && !body.contains(ANONYMOUS),
        "case {:?}: a method whose authorizationType is NONE takes no identity decision, so \
         its answer carries no such authorization refusal, got {body:?}",
        row.case
    );
    assert!(
        !body.contains("UnknownService"),
        "case {:?}: the invoke endpoint must be served by the stage, got {body:?}",
        row.case
    );
}

#[tokio::test]
async fn renamed_path_parameters_address_the_bucket_and_key_the_request_carries() {
    let router = emulator();
    let api = stage_with_renamed_parameters(&router).await;

    let rows = vec![
        Row {
            case: "renamed placeholders naming the first shelf",
            verb: "GET",
            path: format!("/{BUCKET_ONE}/{ALPHA_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(ONE_ALPHA),
            expected_contains: &[],
        },
        Row {
            case: "the same method and the same key, naming the other shelf",
            verb: "GET",
            path: format!("/{BUCKET_TWO}/{ALPHA_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(TWO_ALPHA),
            expected_contains: &[],
        },
        Row {
            case: "placeholders spelled like the method's own parameters",
            verb: "GET",
            path: format!("/direct/{BUCKET_TWO}/{ALPHA_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(TWO_ALPHA),
            expected_contains: &[],
        },
        Row {
            case: "a greedy path parameter renamed onto the key",
            verb: "GET",
            path: format!("/deep/{BUCKET_ONE}/{NESTED_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(ONE_NESTED),
            expected_contains: &[],
        },
        Row {
            case: "the bucket mapped from a header",
            verb: "GET",
            path: format!("/sourced/by-header/{ALPHA_KEY}"),
            headers: vec![(SHELF_HEADER, BUCKET_TWO)],
            body: b"",
            expected_status: 200,
            expected_body: Some(TWO_ALPHA),
            expected_contains: &[],
        },
        Row {
            case: "the bucket mapped from a query parameter",
            verb: "GET",
            path: format!("/sourced/by-query/{ALPHA_KEY}?{SHELF_QUERY}={BUCKET_ONE}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(ONE_ALPHA),
            expected_contains: &[],
        },
        Row {
            case: "a head of the object the renamed parameters address, which carries no body",
            verb: "HEAD",
            path: format!("/{BUCKET_ONE}/{ALPHA_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: Some(b""),
            expected_contains: &[],
        },
        Row {
            case: "a write addressed at the second shelf",
            verb: "PUT",
            path: format!("/{BUCKET_TWO}/{WRITTEN_KEY}"),
            headers: vec![("content-type", TEXT_MEDIA_TYPE)],
            body: WRITTEN_BYTES,
            expected_status: 200,
            expected_body: None,
            expected_contains: &[],
        },
        Row {
            case: "a delete addressed at the first shelf",
            verb: "DELETE",
            path: format!("/{BUCKET_ONE}/{SPARE_KEY}"),
            headers: Vec::new(),
            body: b"",
            expected_status: 200,
            expected_body: None,
            expected_contains: &[],
        },
        Row {
            case: "a key the named bucket holds nothing under",
            verb: "GET",
            path: format!("/{BUCKET_ONE}/{ABSENT_KEY}"),
            headers: Vec::new(),
            body: b"",
            // Amazon S3 answers `NoSuchKey` with 404, and the integration response whose
            // pattern matches that status is what the client sees.
            expected_status: 404,
            expected_body: None,
            expected_contains: &["NoSuchKey"],
        },
        Row {
            case: "a bucket the integration's role is granted nothing on",
            verb: "GET",
            path: format!("/{LOCKED_BUCKET}/{ALPHA_KEY}"),
            headers: Vec::new(),
            body: b"",
            // The refusal is the SERVICE's own, in its own error envelope, and it is
            // about the role's grant rather than about the caller.
            expected_status: 403,
            expected_body: None,
            expected_contains: &["AccessDenied"],
        },
    ];

    for row in &rows {
        let answer = invoke(
            &router,
            &api,
            row.verb,
            &row.path,
            &row.headers,
            Bytes::from(row.body.to_vec()),
        )
        .await;
        assert_row(row, &answer);
    }

    // The locked row carried nothing of the object it did not read.
    let locked = invoke(
        &router,
        &api,
        "GET",
        &format!("/{LOCKED_BUCKET}/{ALPHA_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert!(
        !locked.text().contains(LOCKED_TEXT),
        "a refused call must carry nothing of the object, got {:?}",
        locked.text()
    );

    // ROUND TRIP: the write the stage performed landed in the bucket the request named, read
    // back through Amazon S3's own API — and NOT in the other shelf, which is what makes the
    // bucket parameter the request's rather than the integration's.
    let read_back = s3(
        &router,
        "GET",
        &format!("/{BUCKET_TWO}/{WRITTEN_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        read_back.status,
        200,
        "the object the stage wrote must be readable through Amazon S3, got {} with {:?}",
        read_back.status,
        read_back.text()
    );
    assert_eq!(
        read_back.body.as_ref(),
        WRITTEN_BYTES,
        "Amazon S3 must hold the bytes the stage wrote, got {:?}",
        read_back.text()
    );
    let elsewhere = s3(
        &router,
        "GET",
        &format!("/{BUCKET_ONE}/{WRITTEN_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        elsewhere.status,
        404,
        "the write must reach only the bucket the request named, got {} with {:?}",
        elsewhere.status,
        elsewhere.text()
    );

    // And the delete removed the object from the bucket the request named, leaving the same
    // key in the other shelf where it was.
    let gone = s3(
        &router,
        "GET",
        &format!("/{BUCKET_ONE}/{SPARE_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        gone.status,
        404,
        "the object the stage deleted must be gone from the bucket it named, got {} with {:?}",
        gone.status,
        gone.text()
    );
    let spared = s3(
        &router,
        "GET",
        &format!("/{BUCKET_TWO}/{SPARE_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        spared.status,
        200,
        "the same key in the other shelf must be untouched, got {} with {:?}",
        spared.status,
        spared.text()
    );
    assert_eq!(
        spared.body.as_ref(),
        SPARE_BYTES,
        "the other shelf must still hold its own bytes, got {:?}",
        spared.text()
    );
}

#[tokio::test]
async fn a_none_authorized_method_is_served_whatever_credential_the_request_presents() {
    // The identity half of the rule, over the credential the request carries: a method whose
    // `authorizationType` is `NONE` authenticates nobody, so the same object is served to a
    // caller presenting no credential at all and to one presenting a credential that names a
    // principal no store holds — and neither answer is an `execute-api:Invoke` refusal.
    let router = emulator();
    let api = stage_with_renamed_parameters(&router).await;

    let credential = presented_credential(UNHELD_KEY);
    let cases: [(&str, Vec<(&str, &str)>); 3] = [
        ("no credential at all, as a client of a public method sends it", Vec::new()),
        (
            "a credential naming a principal no store holds",
            vec![("authorization", credential.as_str())],
        ),
        (
            "the same credential with a session token beside it",
            vec![
                ("authorization", credential.as_str()),
                ("x-amz-security-token", "a-token-no-store-minted"),
            ],
        ),
    ];

    for (case, headers) in &cases {
        let answer = invoke(
            &router,
            &api,
            "GET",
            &format!("/{BUCKET_ONE}/{ALPHA_KEY}"),
            headers,
            Bytes::new(),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected the object the renamed parameters address, got {} with {:?}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.body.as_ref(),
            ONE_ALPHA,
            "case {case:?}: expected the stored bytes, got {:?}",
            answer.text()
        );
        let body = answer.text();
        assert!(
            !body.contains(INVOKE_AUTHORIZATION) && !body.contains(ANONYMOUS),
            "case {case:?}: no identity decision is taken for a NONE method, so no \
             authorization refusal can be its answer, got {body:?}"
        );
    }
}
