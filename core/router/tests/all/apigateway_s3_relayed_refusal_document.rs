// SPDX-License-Identifier: CC0-1.0

//! What a deployed method served by an `AWS` integration to Amazon S3 answers when the call
//! that integration makes is REFUSED: the service's own error document, relayed whole.
//!
//! RULE: a method whose integration is of type `AWS` is served by making the call its `uri`
//! names against the service that `uri` names, and the method response carries THAT answer —
//! "For all other HTTP and AWS back ends, the HTTP status code is matched"
//! (`IntegrationResponse$selectionPattern`) — so a refusal reaches the caller as the refusing
//! service's own status and its own error document, never as a body API Gateway invented. In
//! that document the `Code` and the `Message` are the FIXED text of the error class, and the
//! element beside them names the resource the INTEGRATION REQUEST addressed — the object's
//! `Key` for an object-scoped refusal, the `BucketName` for a bucket-scoped one, and
//! `Resource`, "the bucket or object that is involved in the error" (Amazon S3 API Reference,
//! *Error responses*, whose `<Error>` document is `Code`, `Message`, the condition's own
//! contextual elements, `RequestId` and `HostId`) — which for an integration whose `uri`
//! carries `{placeholder}`s is the address its `requestParameters` mapping resolved for THIS
//! request. A call the role's own policies do not allow never reaches the service at all: API
//! Gateway refuses it itself, the way that service refuses an unauthorized call, in the same
//! envelope and naming the same mapped resource.
//!
//! So the parameter mapping is observable on the refusal path as well as on the success path:
//! two requests to one method that differ only in the key they name are answered with two
//! documents that differ only in the key they name back.
//!
//! Only a cross-service suite can pin this: the refusal is Amazon S3's, the integration's role
//! and its grant are IAM's, and the api, the stage and the mapping are API Gateway's, so the
//! rule is observable only where all three are mounted on one engine.
//!
//! The rows vary what the rule generalises over: the error CLASS (a key nothing is stored
//! under, a bucket the account does not hold, a call the role may not make), the shape of the
//! KEY the document echoes (a plain name, a name with no extension, one with dots and dashes,
//! one carrying a character the document has to escape, one under a prefix carried whole by a
//! greedy path parameter), WHICH bucket the request names, the SPELLING of the `uri`'s
//! placeholders, and the FORM the `uri` names the call in (the `path` form with both of its
//! placeholders mapped from differently named method parameters, and the `action` form, which
//! names the bucket and the key itself).
//!
//! The harness is the sibling suite's, which owns the success half of this rule
//! ([`super::apigateway_s3_service_integration`]): one emulator, one reading of each service's
//! wire form, so the suites cannot drift on how a stage, a role or an object is made.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_s3_service_integration::{
    apigw_ok, emulator, invoke, member, policy, resource, role, s3, Answer, APIGATEWAY_TRUST,
    MAPPED_STATUSES, REGION, STAGE,
};

/// The bucket most rows address, and a second one holding different bytes: both are covered by
/// the integration role's grant, which names them by the prefix they share.
const BUCKET_ONE: &str = "relayed-shelf-one";
const BUCKET_TWO: &str = "relayed-shelf-two";

/// A bucket name the account holds nothing under, covered by the same grant — so the refusal
/// its row is answered with is about the bucket rather than about the role.
const ABSENT_BUCKET: &str = "relayed-shelf-absent";

/// A bucket that EXISTS and holds an object, outside the grant — so the refusal its row is
/// answered with is about the role rather than about the bucket or the key.
const LOCKED_BUCKET: &str = "locked-relay-shelf";

/// The key every shelf holds, and the bytes it carries: the positive control, which no refusal
/// row's document may carry any of.
const STORED_KEY: &str = "alpha.txt";
const STORED_TEXT: &str = "the object, as stored";

/// The media type the stored objects are written under.
const TEXT_MEDIA_TYPE: &str = "text/plain";

/// The grant the integration's role carries: the object actions on the two shelves whose names
/// share a prefix, and on nothing in [`LOCKED_BUCKET`].
const GRANTED_ACTIONS: &str = r#""s3:GetObject","s3:PutObject""#;
const GRANTED_RESOURCE: &str = "arn:aws:s3:::relayed-shelf-*/*";

/// The fixed text of each error class a row reaches, as this service publishes it: the code and
/// the sentence beside it (Amazon S3 API Reference, *Error responses*).
const NO_SUCH_KEY: (&str, &str) = ("NoSuchKey", "The specified key does not exist.");
const NO_SUCH_BUCKET: (&str, &str) = ("NoSuchBucket", "The specified bucket does not exist");
const ACCESS_DENIED: (&str, &str) = ("AccessDenied", "Access Denied");

/// The keys the refusal rows address, each a different shape of key name.
const PLAIN_KEY: &str = "missing.txt";
const EXTENSIONLESS_KEY: &str = "missing";
const PUNCTUATED_KEY: &str = "report.v2-final.txt";
const NESTED_KEY: &str = "notes/2026/missing.txt";
const ACTED_KEY: &str = "acted-missing.txt";

/// A key carrying a character that is MARKUP in the document that echoes it: the `&` an XML
/// body has to escape to stay one document. It is a legal path character (RFC 3986 names `&`
/// a sub-delimiter, which a path segment may carry) and a legal object key, so the stage can
/// address it and the document has to carry it escaped.
const MARKUP_KEY: &str = "tea&coffee.txt";

/// That key as the error document spells it.
const MARKUP_KEY_ESCAPED: &str = "tea&amp;coffee.txt";

/// The media type an Amazon S3 REST error document is XML under, which the relayed answer
/// declares because it is the one the service declared.
const XML_MEDIA_TYPE: &str = "application/xml";

/// One row: a request to the stage, and the document the records say it is answered with.
struct Row {
    case: &'static str,
    path: String,
    expected_status: u16,
    /// The `<Code>` and `<Message>` of the error class, which are fixed text.
    class: (&'static str, &'static str),
    /// The element naming what the integration request addressed, built from the request this
    /// row makes rather than from any one recorded answer.
    names: String,
    /// Text the document must NOT carry: for a key that is markup, its unescaped spelling.
    absent: Option<String>,
}

/// One method of `resource_id` whose integration is the Amazon S3 call `uri` names under
/// `credentials`, with `mapped` naming, for each `{placeholder}` of that `uri`, the method
/// request parameter its value is mapped from, and with the integration responses that map
/// each class of answer the service can give.
async fn s3_method(
    router: &MultiServiceRouter,
    api: &str,
    resource_id: &str,
    uri: &str,
    credentials: &str,
    mapped: &[(&str, &str)],
) {
    let case = format!("GET {uri} {mapped:?}");
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET"),
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
    let mut integration = json!({
        "type": "AWS",
        // `integrationHttpMethod` travels under its `jsonName`, which is what
        // `smithy_query(query="operation",operation="PutIntegration")` reports for it.
        "httpMethod": "GET",
        "uri": uri,
        "credentials": credentials,
    });
    if !parameters.is_empty() {
        integration["requestParameters"] = Value::Object(parameters);
    }
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET/integration"),
        Some(integration),
        201,
    )
    .await;
    for (status, pattern) in MAPPED_STATUSES {
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!("/restapis/{api}/resources/{resource_id}/methods/GET/responses/{status}"),
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
                "/restapis/{api}/resources/{resource_id}/methods/GET/integration/responses/{status}"
            ),
            Some(response),
            201,
        )
        .await;
    }
}

/// One bucket holding `STORED_KEY`, written through Amazon S3's own API — so what a stage
/// serves for it, and what the service refuses about keys it does NOT hold, is what that
/// service holds.
async fn shelf(router: &MultiServiceRouter, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateBucket({bucket}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let written = s3(
        router,
        "PUT",
        &format!("/{bucket}/{STORED_KEY}"),
        &[("content-type", TEXT_MEDIA_TYPE)],
        Bytes::from(STORED_TEXT.as_bytes().to_vec()),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "setup: PutObject({bucket}/{STORED_KEY}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
}

/// The stage every row is invoked through, answering the api's id: three buckets, one role this
/// api may assume whose grant covers two of them, and the methods whose integrations address
/// Amazon S3 through mapped placeholders and through the action form.
async fn stage_relaying_s3_refusals(router: &MultiServiceRouter) -> String {
    shelf(router, BUCKET_ONE).await;
    shelf(router, BUCKET_TWO).await;
    shelf(router, LOCKED_BUCKET).await;

    let reader = role(
        router,
        "relayed-refusal-reader",
        APIGATEWAY_TRUST,
        &policy(GRANTED_ACTIONS, GRANTED_RESOURCE),
    )
    .await;

    let api_body = apigw_ok(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "relayed-refusal-document" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_body, "id");
    let root = member("CreateRestApi", &api_body, "rootResourceId");

    // Amazon S3's own REST path with BOTH its bucket and its key left as placeholders, each
    // addressed at whatever the mapping resolves for this request.
    let path_form = format!("arn:aws:apigateway:{REGION}:s3:path/{{bucket}}/{{key}}");

    // `/{folder}/{item}`, whose path parameters are spelled differently from the placeholders
    // they are mapped onto.
    let folder = resource(router, &api, &root, "{folder}").await;
    let item = resource(router, &api, &folder, "{item}").await;
    s3_method(
        router,
        &api,
        &item,
        &path_form,
        &reader,
        &[
            ("bucket", "method.request.path.folder"),
            ("key", "method.request.path.item"),
        ],
    )
    .await;

    // The same two renamed sources onto placeholders spelled ANOTHER way (`{object}` rather
    // than `{key}`): a mapping is read by what it names, so the placeholder's own spelling is
    // the integration's business and no row's outcome turns on it.
    let named = resource(router, &api, &root, "named").await;
    let named_folder = resource(router, &api, &named, "{folder}").await;
    let named_item = resource(router, &api, &named_folder, "{item}").await;
    s3_method(
        router,
        &api,
        &named_item,
        &format!("arn:aws:apigateway:{REGION}:s3:path/{{bucket}}/{{object}}"),
        &reader,
        &[
            ("bucket", "method.request.path.folder"),
            ("object", "method.request.path.item"),
        ],
    )
    .await;

    // A GREEDY path parameter renamed onto the key, which carries a key under a prefix whole.
    let deep = resource(router, &api, &root, "deep").await;
    let deep_box = resource(router, &api, &deep, "{box}").await;
    let deep_trail = resource(router, &api, &deep_box, "{trail+}").await;
    s3_method(
        router,
        &api,
        &deep_trail,
        &path_form,
        &reader,
        &[
            ("bucket", "method.request.path.box"),
            ("key", "method.request.path.trail"),
        ],
    )
    .await;

    // The ACTION form, which names the bucket and the key itself rather than mapping them.
    let acted = resource(router, &api, &root, "acted").await;
    s3_method(
        router,
        &api,
        &acted,
        &format!(
            "arn:aws:apigateway:{REGION}:s3:action/GetObject&Bucket={BUCKET_ONE}&Key={ACTED_KEY}"
        ),
        &reader,
        &[],
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

/// Every assertion a refusal row's answer has to satisfy: the status the refusing service
/// answers, the fixed text of its error class, the element naming the resource the mapped
/// integration request addressed, and that the document carries nothing of the object the call
/// did not read.
fn assert_relayed(row: &Row, answer: &Answer) {
    let body = answer.text();
    assert_eq!(
        answer.status, row.expected_status,
        "case {:?}: expected HTTP {}, got {} with {body:?}",
        row.case, row.expected_status, answer.status
    );
    let (code, message) = row.class;
    for element in [
        "<Error>".to_string(),
        format!("<Code>{code}</Code>"),
        format!("<Message>{message}</Message>"),
        row.names.clone(),
    ] {
        assert!(
            body.contains(&element),
            "case {:?}: expected the service's own error document to carry {element:?}, got \
             {body:?}",
            row.case
        );
    }
    if let Some(absent) = &row.absent {
        assert!(
            !body.contains(absent),
            "case {:?}: the document must carry {absent:?} escaped rather than as markup, got \
             {body:?}",
            row.case
        );
    }
    // The relayed answer declares the media type the refusing service declared for it, so a
    // client reads the document as the XML it is.
    assert_eq!(
        answer.header("content-type").as_deref(),
        Some(XML_MEDIA_TYPE),
        "case {:?}: expected the media type the service answered under, got {:?}",
        row.case,
        answer.header("content-type")
    );
    assert!(
        !body.contains(STORED_TEXT),
        "case {:?}: a refused call must carry nothing of any object, got {body:?}",
        row.case
    );
    assert!(
        !body.contains("Internal server error") && !body.contains("UnknownService"),
        "case {:?}: the refusal must be the service's own rather than a gateway response, got \
         {body:?}",
        row.case
    );
}

#[tokio::test]
async fn a_refused_aws_integration_relays_the_services_own_error_document() {
    let router = emulator();
    let api = stage_relaying_s3_refusals(&router).await;

    let rows = vec![
        Row {
            case: "a key the named shelf holds nothing under",
            path: format!("/{BUCKET_ONE}/{PLAIN_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{PLAIN_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "the same method and shelf, naming a key with no extension: the document \
                   echoes the key this request named",
            path: format!("/{BUCKET_ONE}/{EXTENSIONLESS_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{EXTENSIONLESS_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "a key carrying dots and dashes, in the other shelf",
            path: format!("/{BUCKET_TWO}/{PUNCTUATED_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{PUNCTUATED_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "a key carrying a character the document has to escape",
            path: format!("/{BUCKET_ONE}/{MARKUP_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{MARKUP_KEY_ESCAPED}</Key>"),
            // The unescaped spelling would make the key markup of the envelope carrying it.
            absent: Some(MARKUP_KEY.to_string()),
        },
        Row {
            case: "placeholders spelled another way, mapped from the same renamed sources",
            path: format!("/named/{BUCKET_TWO}/{PLAIN_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{PLAIN_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "a key under a prefix, carried whole by a greedy path parameter",
            path: format!("/deep/{BUCKET_ONE}/{NESTED_KEY}"),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{NESTED_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "the action form, whose uri names the key itself",
            path: "/acted".to_string(),
            expected_status: 404,
            class: NO_SUCH_KEY,
            names: format!("<Key>{ACTED_KEY}</Key>"),
            absent: None,
        },
        Row {
            case: "a bucket the account holds nothing under, which the role's grant does cover",
            path: format!("/{ABSENT_BUCKET}/{PLAIN_KEY}"),
            expected_status: 404,
            class: NO_SUCH_BUCKET,
            names: format!("<BucketName>{ABSENT_BUCKET}</BucketName>"),
            absent: None,
        },
        Row {
            case: "a bucket that exists and holds the key, outside the role's grant",
            path: format!("/{LOCKED_BUCKET}/{STORED_KEY}"),
            expected_status: 403,
            class: ACCESS_DENIED,
            // `Resource` is "the bucket or object that is involved in the error" (Amazon S3
            // API Reference, *Error responses*), so the refusal names the resource the MAPPED
            // integration request addressed — which is what says the mapping was applied
            // before the call was judged.
            names: format!("<Resource>/{LOCKED_BUCKET}/{STORED_KEY}</Resource>"),
            absent: None,
        },
    ];

    for row in &rows {
        let answer = invoke(&router, &api, "GET", &row.path, &[], Bytes::new()).await;
        assert_relayed(row, &answer);
    }

    // Two rows differing only in the key they name are answered with documents differing only
    // in the key they name back: the document is written from the request rather than canned.
    let first = invoke(
        &router,
        &api,
        "GET",
        &format!("/{BUCKET_ONE}/{PLAIN_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    let second = invoke(
        &router,
        &api,
        "GET",
        &format!("/{BUCKET_ONE}/{EXTENSIONLESS_KEY}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert!(
        !first.text().contains(&format!("<Key>{EXTENSIONLESS_KEY}</Key>"))
            && !second.text().contains(&format!("<Key>{PLAIN_KEY}</Key>")),
        "each refusal must name the key its own request addressed, got {:?} and {:?}",
        first.text(),
        second.text()
    );
}

#[tokio::test]
async fn the_relayed_refusal_follows_the_state_the_service_holds() {
    // The same request is answered the refusal while the key is absent and the object once the
    // service holds it: the 404 is computed from Amazon S3's own state, not fixed to the
    // method.
    let router = emulator();
    let api = stage_relaying_s3_refusals(&router).await;
    let path = format!("/{BUCKET_ONE}/{PLAIN_KEY}");

    let before = invoke(&router, &api, "GET", &path, &[], Bytes::new()).await;
    assert_eq!(
        before.status,
        404,
        "while the shelf holds nothing under the key, the stage must relay the service's \
         refusal, got {} with {:?}",
        before.status,
        before.text()
    );

    let appeared = b"put between the two reads".to_vec();
    let written = s3(
        &router,
        "PUT",
        &format!("/{BUCKET_ONE}/{PLAIN_KEY}"),
        &[("content-type", TEXT_MEDIA_TYPE)],
        Bytes::from(appeared.clone()),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "PutObject must succeed, got {} with {:?}",
        written.status,
        written.text()
    );

    let after = invoke(&router, &api, "GET", &path, &[], Bytes::new()).await;
    assert_eq!(
        after.status,
        200,
        "once the service holds the object, the same request must be answered with it, got {} \
         with {:?}",
        after.status,
        after.text()
    );
    assert_eq!(
        after.body.as_ref(),
        appeared.as_slice(),
        "the method response must carry the bytes the service holds, got {:?}",
        after.text()
    );
    assert!(
        !after.text().contains(NO_SUCH_KEY.0),
        "the answer must carry none of the refusal the earlier read relayed, got {:?}",
        after.text()
    );
}
