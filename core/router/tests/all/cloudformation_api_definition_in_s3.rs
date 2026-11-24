// SPDX-License-Identifier: CC0-1.0

//! What a stack whose api declaration holds its OpenAPI DEFINITION in Amazon S3 deploys, and
//! what it reports when that object cannot be read.
//!
//! RULE under test: **an api declaration that states its definition as an Amazon S3 reference
//! — `BodyS3Location`, which `AWS::ApiGatewayV2::Api` and `AWS::ApiGateway::RestApi` publish
//! alike — is created by READING that object and importing the api from the bytes it holds, so
//! the api the owning service holds is the one a declaration stating the same document inline
//! would have imported. An object the read cannot be given fails that resource with the
//! exception the api's own handler raises for a reference it could not materialise — `Unable to
//! parse BodyS3Location`, carrying the diagnostic of the exception it raised, which identified
//! nothing, and the one it was raised as, stating the status the read answered and that control
//! plane's bad-request class — reported under the `HandlerErrorCode` class for a service that
//! FAILED, and the stack does not complete.**
//!
//! Why it matters: a reference is how real templates ship an api definition (`aws
//! cloudformation package` and the CDK emit it, never the inline document), so a reference
//! nothing reads means no imported api can be deployed at all. And the reason is the only place
//! the owning handler's refusal survives into CloudFormation's resource view: a client that
//! deployed, read the failure and has to fix the template cannot otherwise tell a missing
//! object from a malformed document or a permissions failure, and cannot classify the failure
//! from the `HandlerErrorCode` either.
//!
//! Three crates have to agree for the rule to hold (CloudFormation reads the object and makes
//! the import, Amazon S3 answers for the object, and the api's own control plane holds the api
//! and the routes the document declares), so the suite lives here rather than in any one of
//! them.
//!
//! The rows differ along what the rule generalises over: WHICH control plane's api type states
//! the reference, the FORM of the key the reference addresses (a flat key and a nested path),
//! the document the object holds (how many routes it declares, and what they are), and — for
//! the failures — WHAT makes the object unreadable (a key the bucket holds nothing under, a
//! bucket Amazon S3 holds no record of, a reference addressing no object at all).
//!
//! Every expected value is either the document this suite staged in Amazon S3, a value the
//! declaration states, or the wording the answering services publish — never read back out of
//! the answer being checked.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The CloudFormation wire, the stack waits and the request envelope the suite about a completed
// stack's outputs owns, so no two suites drift about what settling is.
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, reported_failures, settled_status, Answered,
    CREATE_COMPLETE, REGION,
};
use crate::query_wire::{elements, members_of, outputs_of};

/// The status a resource its provider could not create records, and the status a stack whose
/// create failed rests in when the request asked for its resources to be KEPT.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// "DO_NOTHING … is equivalent to specifying true for the DisableRollback parameter": the
/// failure action that leaves the failed stack and its resource records in place, which is what
/// a client reads the failed resource's diagnosis off.
const DO_NOTHING: &str = "DO_NOTHING";

/// The `HandlerErrorCode` class a provider reports a failure of its OWN read under, a value of
/// the CloudFormation model's own `HandlerErrorCode` enum: the plugin has no mapping for the
/// exception it was handed, so the class is the one for a service that failed rather than for a
/// request the provider found fault with.
const GENERAL_SERVICE_EXCEPTION: &str = "GeneralServiceException";

/// The status Amazon S3 answers a read of an object it holds no record of, for either absence
/// below. Source: Amazon S3's "Error responses" list — `NoSuchKey` and `NoSuchBucket` are both
/// HTTP 404.
const ABSENT_STATUS: u16 = 404;

/// The status such a diagnostic states where NO answer came back at all: nothing was answered,
/// so there is no status to report.
const NO_ANSWERED_STATUS: u16 = 0;

/// The two api types that publish the reference, and the property it is published under.
const APIGATEWAYV2_API: &str = "AWS::ApiGatewayV2::Api";
const APIGATEWAY_REST_API: &str = "AWS::ApiGateway::RestApi";
const BODY_S3_LOCATION: &str = "BodyS3Location";

/// The services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation reads the staged object and makes the
/// import, Amazon S3 answers for the object, and each api control plane holds the api it
/// imported.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Request authentication is off, as in every other suite here: what is under test is what
    // the services answer, never signature verification.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// Call one Amazon S3 operation over its REST surface, path-style — the addressing the emulated
/// service routes on.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// Call one operation of either API Gateway control plane: both sign `apigateway`, and the v2
/// surface is the one bound under `/v2`.
async fn api_gateway(router: &MultiServiceRouter, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[("authorization", &authorization("apigateway"))],
        Bytes::new(),
    )
    .await
}

/// Create `bucket` and stage every `(key, document)` in it, asserting Amazon S3 took each write.
async fn stage(router: &MultiServiceRouter, case: &str, bucket: &str, objects: &[(&str, &str)]) {
    let created = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected Amazon S3 to create {bucket:?}, got {} with {:?}",
        created.status,
        created.text()
    );
    for (key, document) in objects {
        let staged = s3(router, "PUT", &format!("/{bucket}/{key}"), document).await;
        assert_eq!(
            staged.status,
            200,
            "case {case:?}: expected Amazon S3 to hold {key:?} in {bucket:?}, got {} with {:?}",
            staged.status,
            staged.text()
        );
    }
}

/// `CreateStack` for `template`, with the extra request members a row states, asserting the
/// submission itself was accepted.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    extra: &[(&str, &str)],
) {
    let mut members: Vec<(&str, &str)> = vec![("StackName", stack), ("TemplateBody", template)];
    members.extend_from_slice(extra);
    let created = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack({stack:?}), got {} with {:?}",
        created.status,
        created.text()
    );
}

/// The `(ResourceStatus, ResourceStatusReason)` `DescribeStackResources` reports for one
/// logical id — the per-resource view a client reads a failure off — with the reason empty
/// where the row carries none.
async fn reported_resource(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String) {
    let answered = cloudformation(
        router,
        "DescribeStackResources",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources({stack:?}), got {} with \
         {body:?}",
        answered.status
    );
    let rows: Vec<(String, String)> = members_of(&body)
        .into_iter()
        .filter(|member| {
            elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id)
        })
        .map(|member| {
            (
                elements(member, "ResourceStatus")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
                elements(member, "ResourceStatusReason")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "case {case:?}: expected exactly one {logical_id:?} row in {body:?}, got {rows:?}"
    );
    rows.into_iter().next().expect("the one row asserted above")
}

/// Hold a `ResourceStatusReason` to the envelope CloudFormation reports a resource provider's
/// answer in — the exception quoted, the token of the resource request it answered, and the
/// class that answer falls in — and to the exception and class a row expects.
///
/// The token is the only part of the reason that is not fixed for a failure class, so it is
/// asserted as a value that is there rather than compared.
fn assert_handler_reason(case: &str, reason: &str, exception: &str, class: &str) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = ", HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    assert_eq!(
        quoted, exception,
        "case {case:?}: expected the exception the api's handler raises, got {quoted:?}"
    );
    let (token, reported) = tail.split_once(CLASS).unwrap_or_else(|| {
        panic!("case {case:?}: expected a HandlerErrorCode past the token, got {reason:?}")
    });
    assert!(
        !token.is_empty() && token.chars().all(|held| held.is_ascii_hexdigit() || held == '-'),
        "case {case:?}: expected the token of the resource request the refusal answered, got \
         {token:?}"
    );
    assert_eq!(
        reported.strip_suffix(')'),
        Some(class),
        "case {case:?}: expected the class a provider's own failed read falls in, got {reason:?}"
    );
}

/// The exception the api's handler raises for a reference it could not materialise, as it reads
/// for a read that answered `status` — fixed text but for that status, which is what the read
/// answered.
fn unreadable_reference(status: u16) -> String {
    format!(
        "Unable to parse {BODY_S3_LOCATION} (Service: null; Status Code: {NO_ANSWERED_STATUS}; \
         Error Code: null; Request ID: null; Proxy: null) (Service: null; Status Code: {status}; \
         Error Code: BadRequestException; Request ID: null; Proxy: null)"
    )
}

/// One OpenAPI document this suite stages: the api's title, and one `(path, method, backend)`
/// per route it declares through API Gateway's own integration extension — which is what makes
/// an imported api serve anything.
fn definition(title: &str, routes: &[(&str, &str)]) -> String {
    let mut paths = serde_json::Map::new();
    for (path, method) in routes {
        paths.insert(
            (*path).to_string(),
            json!({
                method.to_ascii_lowercase(): {
                    "x-amazon-apigateway-integration": {
                        "type": "http_proxy",
                        "httpMethod": method,
                        "uri": format!("https://upstream.example.com{path}"),
                        "payloadFormatVersion": "1.0"
                    }
                }
            }),
        );
    }
    json!({
        "openapi": "3.0.1",
        "info": { "title": title, "version": "1.0" },
        "paths": Value::Object(paths)
    })
    .to_string()
}

/// The template an `AWS::ApiGatewayV2::Api` row deploys: the api imported from the object its
/// reference addresses, and the `$default` stage that serves it.
///
/// No `Name` and no `ProtocolType`: "if you specify a Body or BodyS3Location, don't specify …
/// Name, ProtocolType …" — the document carries both.
fn v2_template(location: Value) -> String {
    json!({
        "Resources": {
            "S3Api": {
                "Type": APIGATEWAYV2_API,
                "Properties": { BODY_S3_LOCATION: location }
            },
            "S3ApiStage": {
                "Type": "AWS::ApiGatewayV2::Stage",
                "Properties": {
                    "ApiId": { "Ref": "S3Api" },
                    "StageName": "$default",
                    "AutoDeploy": true
                }
            }
        },
        "Outputs": { "ApiRef": { "Value": { "Ref": "S3Api" } } }
    })
    .to_string()
}

/// The template an `AWS::ApiGateway::RestApi` row deploys: the api imported from the object its
/// reference addresses. The REST plane's api takes its name from the document too, so the
/// declaration states none here either.
fn rest_template(location: Value) -> String {
    json!({
        "Resources": {
            "S3Api": {
                "Type": APIGATEWAY_REST_API,
                "Properties": { BODY_S3_LOCATION: location }
            }
        },
        "Outputs": { "ApiRef": { "Value": { "Ref": "S3Api" } } }
    })
    .to_string()
}

/// One deployed row: which api type states the reference, the key the document is staged
/// under, and what the owning service must then hold.
struct Imported {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    /// The key the row stages its document under, which is what the reference addresses.
    key: &'static str,
    /// The `info.title` the staged document names the api with, which is the name the import
    /// gives it.
    title: &'static str,
    /// The routes the staged document declares, as `(path, method)`.
    routes: &'static [(&'static str, &'static str)],
    /// Whether the row's api is the v2 plane's (and so whether the template declares a stage).
    v2: bool,
}

const IMPORTED: &[Imported] = &[
    Imported {
        // The recorded shape: an HTTP api whose definition is an object staged under a flat
        // key, with the one route the document declares.
        case: "an HTTP api imported from an object under a flat key",
        stack: "apigwv2-s3-definition-flat",
        bucket: "apigwv2-s3-definition-flat",
        key: "good.json",
        title: "imported-from-a-flat-key",
        routes: &[("/ok", "GET")],
        v2: true,
    },
    Imported {
        case: "an HTTP api imported from an object under a nested key, declaring two routes",
        stack: "apigwv2-s3-definition-nested",
        bucket: "apigwv2-s3-definition-nested",
        key: "apis/http/definition.json",
        title: "imported-from-a-nested-key",
        routes: &[("/items", "GET"), ("/orders", "POST")],
        v2: true,
    },
    Imported {
        case: "a REST api imported from an object under a nested key",
        stack: "apigw-rest-s3-definition",
        bucket: "apigw-rest-s3-definition",
        key: "apis/rest/definition.json",
        title: "imported-rest-from-s3",
        routes: &[("/ok", "GET")],
        v2: false,
    },
];

/// Every row: the api the stack deployed is the one the staged document describes — its name,
/// and the routes (or resources) that document declares — read back through the owning
/// service's own read.
#[tokio::test]
async fn an_api_whose_definition_is_staged_in_s3_is_imported_from_that_object() {
    for row in IMPORTED {
        let router = build_router();
        let case = row.case;
        let document = definition(row.title, row.routes);
        stage(&router, case, row.bucket, &[(row.key, &document)]).await;

        let location = json!({ "Bucket": row.bucket, "Key": row.key });
        let template = if row.v2 {
            v2_template(location)
        } else {
            rest_template(location)
        };
        create_stack(&router, case, row.stack, &template, &[]).await;

        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to complete for a definition Amazon S3 serves, \
             got {settled:?}; the failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );
        let outputs = outputs_of(&described);
        let api_id = outputs
            .get("ApiRef")
            .unwrap_or_else(|| panic!("case {case:?}: expected an ApiRef output in {outputs:?}"))
            .clone();

        if row.v2 {
            // 1. The api is there, NAMED by the document the object held: the import is what
            // decided the name, so an api carrying it read that object.
            let read = api_gateway(&router, &format!("/v2/apis/{api_id}")).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: expected GetApi to answer for the published ApiId {api_id:?}, got \
                 {} with {:?}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.json().get("name").and_then(Value::as_str),
                Some(row.title),
                "case {case:?}: expected the api to carry the name the staged document states, \
                 got {}",
                read.text()
            );

            // 2. And it holds exactly the routes that document declares, which is what an
            // imported api is for.
            let read = api_gateway(&router, &format!("/v2/apis/{api_id}/routes")).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: expected GetRoutes to answer for {api_id:?}, got {} with {:?}",
                read.status,
                read.text()
            );
            let mut held: Vec<String> = read
                .json()
                .get("items")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|route| route.get("routeKey").and_then(Value::as_str))
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            held.sort();
            let mut expected: Vec<String> = row
                .routes
                .iter()
                .map(|(path, method)| format!("{method} {path}"))
                .collect();
            expected.sort();
            assert_eq!(
                held, expected,
                "case {case:?}: expected the routes the staged document declares, got {}",
                read.text()
            );

            // 3. The stage the template declared over that imported api is there too, under the
            // name it states — so the stack really deployed an api its dependents could use.
            let read = api_gateway(&router, &format!("/v2/apis/{api_id}/stages/$default")).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: expected the declared stage to be readable, got {} with {:?}",
                read.status,
                read.text()
            );
            continue;
        }

        // The REST plane's own read: the api carries the document's title, and the resource
        // tree the document's paths declare.
        let read = api_gateway(&router, &format!("/restapis/{api_id}")).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetRestApi to answer for the published id {api_id:?}, got {} \
             with {:?}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.json().get("name").and_then(Value::as_str),
            Some(row.title),
            "case {case:?}: expected the api to carry the name the staged document states, got {}",
            read.text()
        );
        let read = api_gateway(&router, &format!("/restapis/{api_id}/resources")).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetResources to answer for {api_id:?}, got {} with {:?}",
            read.status,
            read.text()
        );
        let paths: Vec<String> = read
            .json()
            .get("item")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|held| held.get("path").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for (path, _) in row.routes {
            assert!(
                paths.iter().any(|held| held == path),
                "case {case:?}: expected the resource tree the staged document declares to carry \
                 {path:?}, got {paths:?}"
            );
        }
    }
}

/// What a row makes unreadable about the object its reference addresses.
enum Unstaged {
    /// The bucket and another object are both there, and the reference names a key nothing was
    /// staged under.
    KeyNothingIsUnder,
    /// Nothing is staged at all: the bucket itself is what Amazon S3 holds no record of.
    BucketNothingIsUnder,
    /// The reference names no object at all, so no read is made.
    NoObjectAtAll,
}

/// One failing row: the api type that states the reference, and what makes it unreadable.
struct Unreadable {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    unstaged: Unstaged,
    /// Whether the row's api is the v2 plane's.
    v2: bool,
}

const UNREADABLE: &[Unreadable] = &[
    Unreadable {
        // The recorded shape: the bucket exists and the reference names a key that was never
        // uploaded.
        case: "an HTTP api whose definition names a key the bucket holds nothing under",
        stack: "apigwv2-s3-definition-missing-key",
        bucket: "apigwv2-s3-definition-missing-key",
        unstaged: Unstaged::KeyNothingIsUnder,
        v2: true,
    },
    Unreadable {
        case: "an HTTP api whose definition is in a bucket Amazon S3 holds no record of",
        stack: "apigwv2-s3-definition-missing-bucket",
        bucket: "apigwv2-s3-definition-missing-bucket",
        unstaged: Unstaged::BucketNothingIsUnder,
        v2: true,
    },
    Unreadable {
        case: "an HTTP api whose reference addresses no object at all",
        stack: "apigwv2-s3-definition-no-object",
        bucket: "apigwv2-s3-definition-no-object",
        unstaged: Unstaged::NoObjectAtAll,
        v2: true,
    },
    Unreadable {
        case: "a REST api whose definition names a key the bucket holds nothing under",
        stack: "apigw-rest-s3-definition-missing-key",
        bucket: "apigw-rest-s3-definition-missing-key",
        unstaged: Unstaged::KeyNothingIsUnder,
        v2: false,
    },
];

/// The key a failing row stages a real object under, when it stages one, and the key its
/// reference names instead.
const STAGED_KEY: &str = "uploaded.json";
const NEVER_STAGED_KEY: &str = "never-uploaded.json";

/// Every row: the resource fails with the exception the api's handler raises for a reference it
/// could not materialise, under the class for a service that failed, and the stack does not
/// complete.
#[tokio::test]
async fn an_unreadable_definition_fails_its_api_with_the_read_it_could_not_make() {
    for row in UNREADABLE {
        let router = build_router();
        let case = row.case;
        // What the row stages, the reference its declaration states, and the status the read
        // then answers.
        let (location, status) = match row.unstaged {
            Unstaged::KeyNothingIsUnder => {
                let document = definition("staged-elsewhere", &[("/ok", "GET")]);
                stage(&router, case, row.bucket, &[(STAGED_KEY, &document)]).await;
                (
                    json!({ "Bucket": row.bucket, "Key": NEVER_STAGED_KEY }),
                    ABSENT_STATUS,
                )
            }
            Unstaged::BucketNothingIsUnder => (
                json!({ "Bucket": row.bucket, "Key": STAGED_KEY }),
                ABSENT_STATUS,
            ),
            // A reference stating no key addresses no object, so nothing is read and no status
            // is answered.
            Unstaged::NoObjectAtAll => (json!({ "Bucket": row.bucket }), NO_ANSWERED_STATUS),
        };
        let template = if row.v2 {
            v2_template(location)
        } else {
            rest_template(location)
        };

        // `OnFailure=DO_NOTHING`, so the failed stack keeps its resource records: the
        // per-resource view is where a client reads the failure's diagnosis.
        create_stack(
            &router,
            case,
            row.stack,
            &template,
            &[("OnFailure", DO_NOTHING)],
        )
        .await;

        // 1. The stack does not report success for an api that was never imported.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, CREATE_FAILED,
            "case {case:?}: expected the stack to fail for a definition that cannot be read, got \
             {settled:?}; the failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );

        // 2. ...and the api's own row carries the handler's refusal, which is the only place
        // that refusal reaches a client.
        let (status_reported, reason) = reported_resource(&router, case, row.stack, "S3Api").await;
        assert_eq!(
            status_reported, CREATE_FAILED,
            "case {case:?}: expected the api's row to report {CREATE_FAILED}, got \
             {status_reported:?} with {reason:?}"
        );
        assert_handler_reason(
            case,
            &reason,
            &unreadable_reference(status),
            GENERAL_SERVICE_EXCEPTION,
        );
    }
}
