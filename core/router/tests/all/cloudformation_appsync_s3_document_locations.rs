// SPDX-License-Identifier: CC0-1.0

//! What a stack whose `AWS::AppSync::*` declaration holds its document in AMAZON S3 deploys,
//! and what it reports when that object cannot be read.
//!
//! RULE under test: **a declared property that REFERENCES an Amazon S3 object holding another
//! property's text — `CodeS3Location` beside `Code`, `DefinitionS3Location` beside
//! `Definition`, a `…MappingTemplateS3Location` beside its template, a channel namespace's
//! `CodeS3Location` beside its `CodeHandlers` — is resolved by reading that object and
//! creating the resource with its bytes as that property, so the resource AppSync holds is the
//! one a declaration stating the same text inline would create. An object the read cannot be
//! given fails that resource with the provider's own diagnosis of the read — the document it
//! was materialising, and inside it the reference it was reading and the exception an Amazon S3
//! client raises — reported under the `HandlerErrorCode` class for a service that failed, and
//! rolls the stack back.**
//!
//! Why it matters: a reference is how real templates ship resolver and function code (`aws
//! cloudformation package` and the CDK emit it, never the inline text), so a reference nothing
//! reads means no AppSync pipeline can be deployed at all — and the one failure a client does
//! see has to diagnose their object rather than the emulator.
//!
//! Three crates have to agree for the rule to hold (CloudFormation reads the object and makes
//! the create, Amazon S3 answers for the object, AppSync holds the resource and serves the
//! query), so the suite lives here rather than in any one crate.
//!
//! The rows differ along what the rule generalises over: WHICH reference the declaration states
//! and which property's text it holds, which type publishes it, and — for the failures — WHAT
//! makes the object unreadable (a key the bucket holds nothing under, a bucket that is not
//! there, text that addresses no object at all).
//!
//! Every expected value is either the text this suite staged in Amazon S3, a value the
//! declaration states, or the wording the answering services publish — never read back out of
//! the answer being checked.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE socket client of this target's AppSync data-plane suites: how a client reaches the
// URL an API published.
use crate::appsync_lambda_data_source_and_authorizer::post_published_uri;
// The awsQuery XML readers this crate writes ONCE (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, members_of, outputs_of};
// The CloudFormation wire, the stack waits and the request envelope the suite about a completed
// stack's outputs already owns, so no two suites drift about what settling is.
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, create_stack, dispatch, reported_failures, settled_status,
    Answered, CREATE_COMPLETE, REGION,
};

/// The status a create whose resource failed rolls the stack back to, and the event such a
/// resource reports.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The `HandlerErrorCode` class a provider reports a failure of its OWN read under, a value of
/// the CloudFormation model's own `HandlerErrorCode` enum: the plugin has no mapping for the
/// exception, so the class is the one for a service that failed rather than for a refused
/// request.
const GENERAL_SERVICE_EXCEPTION: &str = "GeneralServiceException";

/// The SDK's own name for Amazon S3, as the exception an S3 client raises states it.
const S3_SERVICE: &str = "S3";

/// The sentences and codes AMAZON S3 publishes for the two absences below, spelled out here
/// rather than taken from the emulator's own constants so the suite pins the wording and not
/// the implementation. Source: Amazon S3's "Error responses" list — `NoSuchKey` ("The specified
/// key does not exist.", HTTP 404) and `NoSuchBucket` ("The specified bucket does not exist",
/// HTTP 404).
const NO_SUCH_KEY_SENTENCE: &str = "The specified key does not exist.";
const NO_SUCH_BUCKET_SENTENCE: &str = "The specified bucket does not exist";
const ABSENT_STATUS: u16 = 404;

/// The APPSYNC_JS runtime a resolver and a function defined by `code` name. Source:
/// `AppSyncRuntime`'s own members (`smithy_query(query="shape",shape="AppSyncRuntime")`).
const APPSYNC_JS: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The documents this suite stages in Amazon S3, each the text of the property whose reference
/// addresses it.
const FUNCTION_CODE: &str = "export function request(ctx) { return {}; }\n\
                             export function response(ctx) { return 's3-function-result'; }\n";
const RESOLVER_CODE: &str = "export function request(ctx) { return {}; }\n\
                             export function response(ctx) { return ctx.prev.result; }\n";
const SCHEMA_DOCUMENT: &str = "type Query { s3CodeTest: String }\nschema { query: Query }\n";
const REQUEST_TEMPLATE: &str =
    "{\"version\":\"2017-02-28\",\"payload\":{\"value\":\"from-the-object\"}}";
const RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result.value)";
const EVENT_HANDLERS: &str = "export function onPublish(ctx) { return ctx.events; }\n";

/// The result the staged function code answers, which the pipeline's response handler passes
/// through — so a query that reads it back read the code the object held.
const FUNCTION_RESULT: &str = "s3-function-result";

/// The keys the documents above are staged under.
const FUNCTION_CODE_KEY: &str = "appsync/func.js";
const RESOLVER_CODE_KEY: &str = "appsync/resolver.js";
const SCHEMA_KEY: &str = "appsync/schema.graphql";
const REQUEST_TEMPLATE_KEY: &str = "appsync/request.vtl";
const RESPONSE_TEMPLATE_KEY: &str = "appsync/response.vtl";
const EVENT_HANDLERS_KEY: &str = "appsync/handlers.js";

/// The services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation reads the staged object and makes the
/// create, Amazon S3 answers for the object, and AppSync holds the resource and opens the
/// endpoint a query reaches it on.
///
/// The services are named rather than the whole emulator assembled, so a case carries only the
/// stores it drives.
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
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
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

/// Call one AWS AppSync operation over the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding states.
async fn appsync(router: &MultiServiceRouter, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("appsync.{REGION}.amazonaws.com"),
        path,
        &[("authorization", &authorization("appsync"))],
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

/// One output of a completed stack, or a panic naming the case.
fn output(case: &str, described: &str, key: &str) -> String {
    let outputs = outputs_of(described);
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// Every `ResourceStatusReason` the stack recorded for `logical_id` reaching `reported`, in
/// document order: the rule is ONE such event, so the count is what a case asserts.
async fn failure_reasons(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Vec<String> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "expected HTTP 200 from DescribeStackEvents({stack:?}), got {} with {body:?}",
        answered.status
    );
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let matching = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status == reported);
            (named && matching)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
        })
        .collect()
}

/// The exception a `ResourceStatusReason` quotes and the `HandlerErrorCode` class it reports
/// beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold the quoted exception to the provider's NESTED diagnosis — the document it was
/// materialising, and inside it the reference it was reading — and answer what the read
/// reported.
fn asserted_read(case: &str, quoted: &str, document: &str, reference: &str) -> String {
    let opens = format!(
        "Error occurred during operation 'Error reading {document} from S3.Error occurred \
         during operation 'Error reading {reference} from S3. Error message: "
    );
    let closes = "'.'.";
    quoted
        .strip_prefix(&opens)
        .and_then(|rest| rest.strip_suffix(closes))
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the read of {reference:?} nested in the materialising \
                 of {document:?}, got {quoted:?}"
            )
        })
}

/// Hold what the read reported to the shape an Amazon S3 client raises — that service's own
/// sentence, then its SDK's diagnostic of the refused call (the name it gives the service, the
/// HTTP status, the two ids an S3 answer carries and the attempts the SDK made) — and answer
/// the sentence.
fn asserted_s3_exception(case: &str, reported: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = reported.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic after Amazon S3's own sentence, got \
             {reported:?}"
        )
    });
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the attempt count this SDK words, got {reported:?}")
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        4,
        "case {case:?}: expected the service, the status and both ids an Amazon S3 answer \
         carries, got {envelope:?}"
    );
    assert_eq!(
        fields[0], S3_SERVICE,
        "case {case:?}: expected the SDK's own name for Amazon S3, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status Amazon S3 answered, got {:?}",
        fields[1]
    );
    for (field, label) in [(fields[2], "Request ID: "), (fields[3], "Extended Request ID: ")] {
        let id = field.strip_prefix(label).unwrap_or_else(|| {
            panic!("case {case:?}: expected a {label:?} in the diagnostic, got {field:?}")
        });
        assert!(
            !id.is_empty() && id.chars().all(|held| held.is_ascii_alphanumeric()),
            "case {case:?}: expected the opaque id an Amazon S3 answer carries, got {id:?}"
        );
    }
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// The template the end-to-end case deploys: an API-key GraphQL API whose SCHEMA, whose
/// pipeline FUNCTION's code and whose pipeline RESOLVER's code are each an Amazon S3 object the
/// declaration references, and nothing inline but the structure.
fn pipeline_template() -> String {
    json!({
        "Parameters": { "CodeBucket": { "Type": "String" } },
        "Resources": {
            "Api": {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": { "Name": "s3-code-api", "AuthenticationType": "API_KEY" },
            },
            "Key": {
                "Type": "AWS::AppSync::ApiKey",
                "Properties": { "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] } },
            },
            "Schema": {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "DefinitionS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{SCHEMA_KEY}"),
                    },
                },
            },
            "NoneDataSource": {
                "Type": "AWS::AppSync::DataSource",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Name": "NoneDS",
                    "Type": "NONE",
                },
            },
            "Func1": {
                "Type": "AWS::AppSync::FunctionConfiguration",
                "DependsOn": ["Schema", "NoneDataSource"],
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Name": "S3Function",
                    "DataSourceName": { "Fn::GetAtt": ["NoneDataSource", "Name"] },
                    "Runtime": { "Name": APPSYNC_JS.0, "RuntimeVersion": APPSYNC_JS.1 },
                    "CodeS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{FUNCTION_CODE_KEY}"),
                    },
                },
            },
            "PipelineResolver": {
                "Type": "AWS::AppSync::Resolver",
                "DependsOn": ["Schema"],
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "TypeName": "Query",
                    "FieldName": "s3CodeTest",
                    "Kind": "PIPELINE",
                    "PipelineConfig": {
                        "Functions": [{ "Fn::GetAtt": ["Func1", "FunctionId"] }],
                    },
                    "Runtime": { "Name": APPSYNC_JS.0, "RuntimeVersion": APPSYNC_JS.1 },
                    "CodeS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{RESOLVER_CODE_KEY}"),
                    },
                },
            },
        },
        "Outputs": {
            "ApiId": { "Value": { "Fn::GetAtt": ["Api", "ApiId"] } },
            "GraphQLUrl": { "Value": { "Fn::GetAtt": ["Api", "GraphQLUrl"] } },
            "ApiKey": { "Value": { "Fn::GetAtt": ["Key", "ApiKey"] } },
            "FunctionId": { "Value": { "Fn::GetAtt": ["Func1", "FunctionId"] } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Func1", "FunctionArn"] } },
            "ResolverArn": { "Value": { "Fn::GetAtt": ["PipelineResolver", "ResolverArn"] } },
        },
    })
    .to_string()
}

/// The recorded shape: a pipeline whose schema, function code and resolver code are all staged
/// Amazon S3 objects deploys, AppSync reports each document byte for byte, the identities the
/// stack published are the ones that service minted, and a query through the pipeline returns
/// what the staged function code answers.
#[tokio::test]
async fn a_pipeline_whose_documents_are_staged_in_s3_deploys_and_serves_its_query() {
    let case = "a schema, a function's code and a resolver's code, each in an S3 object";
    let router = build_router();
    let bucket = "appsync-s3-code-pipeline";
    let stack = "appsync-s3-code-pipeline";

    stage(
        &router,
        case,
        bucket,
        &[
            (SCHEMA_KEY, SCHEMA_DOCUMENT),
            (FUNCTION_CODE_KEY, FUNCTION_CODE),
            (RESOLVER_CODE_KEY, RESOLVER_CODE),
        ],
    )
    .await;

    create_stack(
        &router,
        case,
        stack,
        &pipeline_template(),
        &[("CodeBucket", bucket)],
    )
    .await;
    let (settled, described) = settled_status(&router, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to complete for documents Amazon S3 serves, got \
         {settled:?}; the failures it reported: {:?}; last answer {described}",
        reported_failures(&router, stack).await
    );

    let api_id = output(case, &described, "ApiId");
    let function_id = output(case, &described, "FunctionId");

    // 1. The FUNCTION AppSync holds carries the code the object held, byte for byte, and the
    // identities the stack published are that service's own.
    let read = appsync(&router, &format!("/v1/apis/{api_id}/functions/{function_id}")).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected AppSync to hold the function the stack published \
         ({function_id:?}), got {} with {:?}",
        read.status,
        read.text()
    );
    let function = read.json().get("functionConfiguration").cloned().unwrap_or(Value::Null);
    assert_eq!(
        function.get("code").and_then(Value::as_str),
        Some(FUNCTION_CODE),
        "case {case:?}: expected the function's code to be the staged object, got {function}"
    );
    assert_eq!(
        function.get("functionArn").and_then(Value::as_str),
        Some(output(case, &described, "FunctionArn").as_str()),
        "case {case:?}: expected the published FunctionArn to be the one AppSync minted, got \
         {function}"
    );

    // 2. So does the RESOLVER, which names that function as its one pipeline step.
    let read = appsync(
        &router,
        &format!("/v1/apis/{api_id}/types/Query/resolvers/s3CodeTest"),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected AppSync to hold the resolver the stack created, got {} with \
         {:?}",
        read.status,
        read.text()
    );
    let resolver = read.json().get("resolver").cloned().unwrap_or(Value::Null);
    assert_eq!(
        resolver.get("code").and_then(Value::as_str),
        Some(RESOLVER_CODE),
        "case {case:?}: expected the resolver's code to be the staged object, got {resolver}"
    );
    assert_eq!(
        resolver.get("resolverArn").and_then(Value::as_str),
        Some(output(case, &described, "ResolverArn").as_str()),
        "case {case:?}: expected the published ResolverArn to be the one AppSync minted, got \
         {resolver}"
    );
    assert_eq!(
        resolver
            .get("pipelineConfig")
            .and_then(|config| config.get("functions"))
            .cloned(),
        Some(json!([function_id])),
        "case {case:?}: expected the pipeline to name the function the stack published, got \
         {resolver}"
    );

    // 3. And the API really SERVES the field: the schema came from one object, the function
    // that answers it from another, and the resolver that passes the answer through from a
    // third.
    let (status, answered) = post_published_uri(
        case,
        &output(case, &described, "GraphQLUrl"),
        &json!({ "query": "query { s3CodeTest }" }),
        &[("x-api-key", output(case, &described, "ApiKey"))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the published endpoint to serve the query, got {status} with \
         {answered}"
    );
    assert_eq!(
        answered
            .get("data")
            .and_then(|data| data.get("s3CodeTest"))
            .and_then(Value::as_str),
        Some(FUNCTION_RESULT),
        "case {case:?}: expected the field to answer what the staged function code returns, \
         got {answered}"
    );
}

/// One row of the table below: a template whose declaration references staged objects, and what
/// AppSync must then report for the resource it created.
struct Row {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    /// The template the row deploys, which takes the bucket as a parameter.
    template: fn() -> String,
    /// The objects the row stages before it deploys.
    objects: &'static [(&'static str, &'static str)],
    /// The output the row reads the resource's own read path off, and the read itself: the
    /// output's value filled into the path, the member the answer carries the resource in, and
    /// the `(member, document)` pairs the read must report.
    reads: &'static [Read],
}

/// One read of a created resource, and the documents it must report.
struct Read {
    /// `{apiId}` and `{output}` are filled in from the stack's own outputs.
    path: &'static str,
    /// The output `{output}` names, where the path carries one.
    output: &'static str,
    /// The member the answer carries the resource in.
    member: &'static str,
    /// Each `(member of the resource, the document the object held)`.
    documents: &'static [(&'static str, &'static str)],
}

/// A UNIT resolver whose two VTL mapping templates are both staged objects, on an API whose
/// schema is declared inline — so what the row varies is only which reference it states.
fn mapping_template_resolver() -> String {
    json!({
        "Parameters": { "CodeBucket": { "Type": "String" } },
        "Resources": {
            "Api": {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": { "Name": "s3-template-api", "AuthenticationType": "API_KEY" },
            },
            "Schema": {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Definition": SCHEMA_DOCUMENT,
                },
            },
            "NoneDataSource": {
                "Type": "AWS::AppSync::DataSource",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Name": "NoneDS",
                    "Type": "NONE",
                },
            },
            "UnitResolver": {
                "Type": "AWS::AppSync::Resolver",
                "DependsOn": ["Schema", "NoneDataSource"],
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "TypeName": "Query",
                    "FieldName": "s3CodeTest",
                    "Kind": "UNIT",
                    "DataSourceName": { "Fn::GetAtt": ["NoneDataSource", "Name"] },
                    "RequestMappingTemplateS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{REQUEST_TEMPLATE_KEY}"),
                    },
                    "ResponseMappingTemplateS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{RESPONSE_TEMPLATE_KEY}"),
                    },
                },
            },
        },
        "Outputs": { "ApiId": { "Value": { "Fn::GetAtt": ["Api", "ApiId"] } } },
    })
    .to_string()
}

/// A pipeline FUNCTION whose two VTL mapping templates are both staged objects.
fn mapping_template_function() -> String {
    json!({
        "Parameters": { "CodeBucket": { "Type": "String" } },
        "Resources": {
            "Api": {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": { "Name": "s3-template-function-api", "AuthenticationType": "API_KEY" },
            },
            "Schema": {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Definition": SCHEMA_DOCUMENT,
                },
            },
            "NoneDataSource": {
                "Type": "AWS::AppSync::DataSource",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Name": "NoneDS",
                    "Type": "NONE",
                },
            },
            "Func1": {
                "Type": "AWS::AppSync::FunctionConfiguration",
                "DependsOn": ["Schema", "NoneDataSource"],
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Api", "ApiId"] },
                    "Name": "TemplateFunction",
                    "DataSourceName": { "Fn::GetAtt": ["NoneDataSource", "Name"] },
                    "FunctionVersion": "2018-05-29",
                    "RequestMappingTemplateS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{REQUEST_TEMPLATE_KEY}"),
                    },
                    "ResponseMappingTemplateS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{RESPONSE_TEMPLATE_KEY}"),
                    },
                },
            },
        },
        "Outputs": {
            "ApiId": { "Value": { "Fn::GetAtt": ["Api", "ApiId"] } },
            "FunctionId": { "Value": { "Fn::GetAtt": ["Func1", "FunctionId"] } },
        },
    })
    .to_string()
}

/// An Event API's channel namespace whose handler code is a staged object: the one type whose
/// reference holds the text of a property not named after it (`CodeHandlers`).
fn channel_namespace_handlers() -> String {
    json!({
        "Parameters": { "CodeBucket": { "Type": "String" } },
        "Resources": {
            "Events": {
                "Type": "AWS::AppSync::Api",
                "Properties": {
                    "Name": "s3-handlers-events",
                    "EventConfig": {
                        "AuthProviders": [{ "AuthType": "API_KEY" }],
                        "ConnectionAuthModes": [{ "AuthType": "API_KEY" }],
                        "DefaultPublishAuthModes": [{ "AuthType": "API_KEY" }],
                        "DefaultSubscribeAuthModes": [{ "AuthType": "API_KEY" }],
                    },
                },
            },
            "Default": {
                "Type": "AWS::AppSync::ChannelNamespace",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["Events", "ApiId"] },
                    "Name": "default",
                    "CodeS3Location": {
                        "Fn::Sub": format!("s3://${{CodeBucket}}/{EVENT_HANDLERS_KEY}"),
                    },
                },
            },
        },
        "Outputs": { "ApiId": { "Value": { "Fn::GetAtt": ["Events", "ApiId"] } } },
    })
    .to_string()
}

const ROWS: &[Row] = &[
    Row {
        case: "a resolver's request and response mapping templates, each in an object",
        stack: "appsync-s3-resolver-templates",
        bucket: "appsync-s3-resolver-templates",
        template: mapping_template_resolver,
        objects: &[
            (REQUEST_TEMPLATE_KEY, REQUEST_TEMPLATE),
            (RESPONSE_TEMPLATE_KEY, RESPONSE_TEMPLATE),
        ],
        reads: &[Read {
            path: "/v1/apis/{apiId}/types/Query/resolvers/s3CodeTest",
            output: "",
            member: "resolver",
            documents: &[
                ("requestMappingTemplate", REQUEST_TEMPLATE),
                ("responseMappingTemplate", RESPONSE_TEMPLATE),
            ],
        }],
    },
    Row {
        case: "a pipeline function's request and response mapping templates, each in an object",
        stack: "appsync-s3-function-templates",
        bucket: "appsync-s3-function-templates",
        template: mapping_template_function,
        objects: &[
            (REQUEST_TEMPLATE_KEY, REQUEST_TEMPLATE),
            (RESPONSE_TEMPLATE_KEY, RESPONSE_TEMPLATE),
        ],
        reads: &[Read {
            path: "/v1/apis/{apiId}/functions/{output}",
            output: "FunctionId",
            member: "functionConfiguration",
            documents: &[
                ("requestMappingTemplate", REQUEST_TEMPLATE),
                ("responseMappingTemplate", RESPONSE_TEMPLATE),
            ],
        }],
    },
    Row {
        case: "a channel namespace's handler code in an object",
        stack: "appsync-s3-namespace-handlers",
        bucket: "appsync-s3-namespace-handlers",
        template: channel_namespace_handlers,
        objects: &[(EVENT_HANDLERS_KEY, EVENT_HANDLERS)],
        reads: &[Read {
            path: "/v2/apis/{apiId}/channelNamespaces/default",
            output: "",
            member: "channelNamespace",
            documents: &[("codeHandlers", EVENT_HANDLERS)],
        }],
    },
];

/// Every row: the stack completes and the document AppSync reports for the created resource is
/// the object the declaration referenced, byte for byte.
#[tokio::test]
async fn every_referenced_document_is_created_as_the_property_whose_text_it_is() {
    let router = build_router();
    for row in ROWS {
        let case = row.case;
        stage(&router, case, row.bucket, row.objects).await;
        create_stack(
            &router,
            case,
            row.stack,
            &(row.template)(),
            &[("CodeBucket", row.bucket)],
        )
        .await;
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to complete for documents Amazon S3 serves, got \
             {settled:?}; the failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );
        let api_id = output(case, &described, "ApiId");
        for read in row.reads {
            let path = read.path.replace("{apiId}", &api_id).replace(
                "{output}",
                &if read.output.is_empty() {
                    String::new()
                } else {
                    output(case, &described, read.output)
                },
            );
            let answered = appsync(&router, &path).await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected AppSync to hold the resource the stack created at \
                 {path:?}, got {} with {:?}",
                answered.status,
                answered.text()
            );
            let held = answered.json().get(read.member).cloned().unwrap_or(Value::Null);
            for (member, document) in read.documents {
                assert_eq!(
                    held.get(member).and_then(Value::as_str),
                    Some(*document),
                    "case {case:?}: expected {member:?} to be the staged object, got {held}"
                );
            }
        }
    }
}

/// What a row makes of the object its reference addresses.
#[derive(Clone, Copy)]
enum Staged {
    /// The bucket is there and holds one object; the reference names another key.
    KeyNothingIsUnder,
    /// No bucket of that name exists at all.
    BucketNothingIsUnder,
    /// The reference carries text that addresses no object at all.
    NoObjectAtAll,
}

/// One row of the refusal table: the reference the declaration states, what makes its object
/// unreadable, and what the provider must then report.
struct Unreadable {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    /// The resource whose declaration states the reference, and the reference itself.
    logical_id: &'static str,
    reference: &'static str,
    /// The property whose text the reference holds, which the provider was materialising.
    document: &'static str,
    /// The template the row deploys, taking the location as a parameter.
    template: fn() -> String,
    staged: Staged,
}

/// A pipeline function whose `CodeS3Location` is whatever location the deployment passes.
fn function_code_location() -> String {
    located(
        "Func1",
        "AWS::AppSync::FunctionConfiguration",
        json!({
            "Name": "LocatedFunction",
            "DataSourceName": { "Fn::GetAtt": ["NoneDataSource", "Name"] },
            "Runtime": { "Name": APPSYNC_JS.0, "RuntimeVersion": APPSYNC_JS.1 },
            "CodeS3Location": { "Ref": "CodeLocation" },
        }),
    )
}

/// The same function, whose `RequestMappingTemplateS3Location` is that location instead.
fn function_template_location() -> String {
    located(
        "Func1",
        "AWS::AppSync::FunctionConfiguration",
        json!({
            "Name": "LocatedFunction",
            "DataSourceName": { "Fn::GetAtt": ["NoneDataSource", "Name"] },
            "FunctionVersion": "2018-05-29",
            "RequestMappingTemplateS3Location": { "Ref": "CodeLocation" },
            "ResponseMappingTemplate": RESPONSE_TEMPLATE,
        }),
    )
}

/// A pipeline resolver whose `CodeS3Location` is that location.
fn resolver_code_location() -> String {
    located(
        "PipelineResolver",
        "AWS::AppSync::Resolver",
        json!({
            "TypeName": "Query",
            "FieldName": "s3CodeTest",
            "Kind": "PIPELINE",
            "Runtime": { "Name": APPSYNC_JS.0, "RuntimeVersion": APPSYNC_JS.1 },
            "CodeS3Location": { "Ref": "CodeLocation" },
        }),
    )
}

/// The template the refusal rows share: an API, its schema and a `NONE` data source declared
/// inline, and ONE resource whose own declaration states the reference under test — so what
/// differs between rows is only the reference and the object it addresses.
fn located(logical_id: &str, resource_type: &str, mut properties: Value) -> String {
    let api_id = json!({ "Fn::GetAtt": ["Api", "ApiId"] });
    if let Some(stated) = properties.as_object_mut() {
        stated.insert("ApiId".to_string(), api_id.clone());
    }
    json!({
        "Parameters": { "CodeLocation": { "Type": "String" } },
        "Resources": {
            "Api": {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": { "Name": "s3-unreadable-api", "AuthenticationType": "API_KEY" },
            },
            "Schema": {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": { "ApiId": api_id, "Definition": SCHEMA_DOCUMENT },
            },
            "NoneDataSource": {
                "Type": "AWS::AppSync::DataSource",
                "Properties": { "ApiId": api_id, "Name": "NoneDS", "Type": "NONE" },
            },
            logical_id: {
                "Type": resource_type,
                "DependsOn": ["Schema", "NoneDataSource"],
                "Properties": properties,
            },
        },
    })
    .to_string()
}

const UNREADABLE: &[Unreadable] = &[
    Unreadable {
        // The recorded shape: the bucket and another object are both there, and the reference
        // names a key nothing was staged under.
        case: "a function's code under a key the bucket holds nothing under",
        stack: "appsync-s3-code-missing-key",
        bucket: "appsync-s3-code-missing-key",
        logical_id: "Func1",
        reference: "CodeS3Location",
        document: "Code",
        template: function_code_location,
        staged: Staged::KeyNothingIsUnder,
    },
    Unreadable {
        case: "a resolver's code in a bucket Amazon S3 holds no record of",
        stack: "appsync-s3-code-missing-bucket",
        bucket: "appsync-s3-code-missing-bucket",
        logical_id: "PipelineResolver",
        reference: "CodeS3Location",
        document: "Code",
        template: resolver_code_location,
        staged: Staged::BucketNothingIsUnder,
    },
    Unreadable {
        case: "a function's request mapping template under a key nothing is under",
        stack: "appsync-s3-template-missing-key",
        bucket: "appsync-s3-template-missing-key",
        logical_id: "Func1",
        reference: "RequestMappingTemplateS3Location",
        document: "RequestMappingTemplate",
        template: function_template_location,
        staged: Staged::KeyNothingIsUnder,
    },
    Unreadable {
        case: "a function's code reference that addresses no object at all",
        stack: "appsync-s3-code-no-object",
        bucket: "appsync-s3-code-no-object",
        logical_id: "Func1",
        reference: "CodeS3Location",
        document: "Code",
        template: function_code_location,
        staged: Staged::NoObjectAtAll,
    },
];

/// The key a refusal row stages a real object under, when it stages one.
const STAGED_KEY: &str = "appsync/staged.js";

/// Every row: the resource fails with the provider's own diagnosis of the read it could not
/// make, under the class for a service that failed, and the stack rolls back.
#[tokio::test]
async fn an_unreadable_reference_fails_its_resource_with_the_read_it_could_not_make() {
    let router = build_router();
    for row in UNREADABLE {
        let case = row.case;
        // What the row stages, the location its declaration states, and what the read must then
        // report.
        let (location, expected): (String, Option<(&str, u16)>) = match row.staged {
            Staged::KeyNothingIsUnder => {
                stage(&router, case, row.bucket, &[(STAGED_KEY, FUNCTION_CODE)]).await;
                (
                    format!("s3://{}/appsync/never-staged.js", row.bucket),
                    Some((NO_SUCH_KEY_SENTENCE, ABSENT_STATUS)),
                )
            }
            // Nothing is staged at all: the bucket itself is what Amazon S3 holds no record of.
            Staged::BucketNothingIsUnder => (
                format!("s3://{}/{STAGED_KEY}", row.bucket),
                Some((NO_SUCH_BUCKET_SENTENCE, ABSENT_STATUS)),
            ),
            Staged::NoObjectAtAll => ("not-an-s3-location".to_string(), None),
        };

        create_stack(
            &router,
            case,
            row.stack,
            &(row.template)(),
            &[("CodeLocation", &location)],
        )
        .await;

        // 1. The stack does not report success: a resource the provider could not create rolls
        // the whole deployment back.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll back for a document that cannot be read, \
             got {settled:?}; the failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );

        // 2. The resource's failed event quotes the provider's nested diagnosis and reports the
        // class a failed read of its own falls in.
        let reasons = failure_reasons(&router, row.stack, row.logical_id, CREATE_FAILED).await;
        assert!(
            !reasons.is_empty(),
            "case {case:?}: expected a {CREATE_FAILED} reason for {:?}, got none",
            row.logical_id
        );
        for reason in &reasons {
            let (quoted, class) = quoted_and_class(case, reason);
            assert_eq!(
                class, GENERAL_SERVICE_EXCEPTION,
                "case {case:?}: expected the class a provider's own failed read falls in, got \
                 {class:?} in {reason:?}"
            );
            let reported = asserted_read(case, &quoted, row.document, row.reference);
            match expected {
                // An Amazon S3 refusal is reported as the exception an S3 client raises: that
                // service's sentence and its SDK's diagnostic of the call.
                Some((sentence, status)) => assert_eq!(
                    asserted_s3_exception(case, &reported, status),
                    sentence,
                    "case {case:?}: expected the sentence Amazon S3 words for this absence, got \
                     {reported:?}"
                ),
                // Text that addresses no object was never a reference to one, which is what the
                // sentence about the member states — there is no S3 answer to report.
                None => assert_eq!(
                    reported,
                    format!(
                        "{} must reference a valid S3 object to which you have access.",
                        row.reference
                    ),
                    "case {case:?}: expected the sentence for a member referencing no readable \
                     object, got {reported:?}"
                ),
            }
        }
    }
}
