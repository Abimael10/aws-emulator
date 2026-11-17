// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack PUBLISHES for an `AWS::AppSync::*` estate, and what a
//! re-deploy of that estate does to it.
//!
//! Three rules, each asserted over a table that spans the class it generalises over rather
//! than the one template that exposed the defect:
//!
//! 1. **Every output a deployed template declares is reported with BOTH halves.** A stack in
//!    a terminal `*_COMPLETE` status reports, for each declared output, an entry carrying
//!    its `OutputKey` and its `OutputValue`, with the `Ref` or `Fn::GetAtt` behind it already
//!    resolved to a concrete value — so the client idiom that reads the outputs as a map
//!    (`{OutputKey: OutputValue}`) finds every one of them. A row of that list carrying a key
//!    and no value is the defect this suite pins: a stack that reports success while a value
//!    it published cannot be dereferenced.
//! 2. **The identity each type publishes is the handle every later call on the resource
//!    takes.** `Ref` and the stack's `PhysicalResourceId` are the same value, and it is the
//!    ARN AppSync built for the API, the data source, the resolver, the function and the API
//!    KEY — never the key a caller presents — and, for a schema, the API's id with the type's
//!    own segment appended, AppSync holding one schema per API.
//! 3. **A changed declaration of a property the owning update call takes is applied IN
//!    PLACE.** The resource keeps its physical id, the stack reaches `UPDATE_COMPLETE`, and
//!    AppSync reports the new configuration; a change to a value that type's identity is
//!    built from REPLACES the resource instead, and an `UpdateReplacePolicy` of `Retain`
//!    leaves the replaced one where it is.
//!
//! No expected value is read out of the answer being checked: each is either what the
//! template declares, or what AppSync — the service that OWNS the resource — reports for it.
//! Two crates have to agree for the rules to hold (CloudFormation makes the calls, AppSync
//! serves them), so the suite lives here rather than in either one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{
    block_of, element, elements, form_encode, members_of, opt_element, outputs_of,
    physical_ids_of,
};

/// How long one `DescribeStacks` poll waits before the next, and how many are made.
///
/// A stack's own status transitions, and the schema creation AppSync carries out
/// asynchronously, are owed on their clocks, so the suite waits for the STATUS rather than
/// for a span, with a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 600;

/// The Region this suite deploys in.
const REGION: &str = "eu-central-1";

/// The stack name every case deploys under.
const STACK: &str = "appsync-published";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body().as_ref()).to_string(),
    )
}

/// Read one AWS AppSync resource over the `restJson1` wire its model declares, on the URI
/// that operation's own `@http` binding states.
async fn appsync(router: &MultiServiceRouter, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appsync"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The same, as the TEXT the answer carries — `GetIntrospectionSchema` answers with the
/// schema document itself rather than with a JSON structure.
async fn appsync_text(router: &MultiServiceRouter, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appsync"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body().as_ref()).to_string(),
    )
}

/// Wait for the stack to report `wanted`, looking again until it does or the deadline passes.
/// A stack that reaches another TERMINAL status fails the suite at once, quoting the answer:
/// the reason a resource failed is in it.
async fn wait_for_stack_status(router: &MultiServiceRouter, wanted: &str) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported_status == wanted {
            return body;
        }
        assert!(
            !reported_status.ends_with("_FAILED") && !reported_status.starts_with("ROLLBACK"),
            "expected the stack to reach {wanted:?}, got {reported_status:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the stack never reached {wanted:?}; last answer was {body}");
}

/// One output of the answer, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The schema the stack installs on its API.
const SCHEMA: &str = "type Note { id: ID! body: String } \
                      type Query { note(id: ID!): Note } \
                      schema { query: Query }";

/// The schema the UPDATE installs, which declares one field more.
const WIDER_SCHEMA: &str = "type Note { id: ID! body: String author: String } \
                            type Query { note(id: ID!): Note notes: [Note] } \
                            schema { query: Query }";

/// The APPSYNC_JS code the resolver carries, and the code the update changes it to.
const RESOLVER_CODE: &str = "export function request(ctx) { return {payload: {id: ctx.args.id}}; } \
                             export function response(ctx) { return ctx.result; }";
const WIDER_RESOLVER_CODE: &str =
    "export function request(ctx) { return {payload: {id: ctx.args.id, body: \"v2\"}}; } \
     export function response(ctx) { return ctx.result; }";

/// The name the data source is created under, and the name the replacement case moves it to.
const SOURCE_NAME: &str = "LocalSource";
const REPLACED_SOURCE_NAME: &str = "MovedSource";

/// The declared output keys and the intrinsic each one reads its resource with.
///
/// Every published attribute of every type the template declares, plus each type's `Ref`:
/// the attribute lists are the resource types' own
/// (`AWS::AppSync::GraphQLApi` publishes `ApiId`, `Arn`, `GraphQLDns`, `GraphQLUrl`,
/// `RealtimeDns`, `RealtimeUrl`; a data source `DataSourceArn` and `Name`; a resolver
/// `ResolverArn`, `TypeName` and `FieldName`; a function `FunctionArn`, `FunctionId`,
/// `DataSourceName` and `Name`; an API key `ApiKey`, `ApiKeyId` and `Arn`; a schema none).
const DECLARED_OUTPUTS: &[(&str, &str)] = &[
    ("ApiRef", r#"{"Ref":"Api"}"#),
    ("ApiIdOut", r#"{"Fn::GetAtt":["Api","ApiId"]}"#),
    ("ApiArn", r#"{"Fn::GetAtt":["Api","Arn"]}"#),
    ("ApiGraphqlDns", r#"{"Fn::GetAtt":["Api","GraphQLDns"]}"#),
    ("ApiGraphqlUrl", r#"{"Fn::GetAtt":["Api","GraphQLUrl"]}"#),
    ("ApiRealtimeDns", r#"{"Fn::GetAtt":["Api","RealtimeDns"]}"#),
    ("ApiRealtimeUrl", r#"{"Fn::GetAtt":["Api","RealtimeUrl"]}"#),
    ("SchemaRef", r#"{"Ref":"Schema"}"#),
    ("SourceRef", r#"{"Ref":"Source"}"#),
    ("SourceArn", r#"{"Fn::GetAtt":["Source","DataSourceArn"]}"#),
    ("SourceName", r#"{"Fn::GetAtt":["Source","Name"]}"#),
    ("FunctionRef", r#"{"Ref":"Enrich"}"#),
    ("FunctionArn", r#"{"Fn::GetAtt":["Enrich","FunctionArn"]}"#),
    ("FunctionId", r#"{"Fn::GetAtt":["Enrich","FunctionId"]}"#),
    ("FunctionName", r#"{"Fn::GetAtt":["Enrich","Name"]}"#),
    (
        "FunctionSource",
        r#"{"Fn::GetAtt":["Enrich","DataSourceName"]}"#,
    ),
    ("ResolverRef", r#"{"Ref":"Resolve"}"#),
    ("ResolverArn", r#"{"Fn::GetAtt":["Resolve","ResolverArn"]}"#),
    ("ResolverType", r#"{"Fn::GetAtt":["Resolve","TypeName"]}"#),
    ("ResolverField", r#"{"Fn::GetAtt":["Resolve","FieldName"]}"#),
    ("KeyRef", r#"{"Ref":"Key"}"#),
    ("KeyId", r#"{"Fn::GetAtt":["Key","ApiKeyId"]}"#),
    ("KeyValue", r#"{"Fn::GetAtt":["Key","ApiKey"]}"#),
    ("KeyArn", r#"{"Fn::GetAtt":["Key","Arn"]}"#),
];

/// The template the suite deploys: one GraphQL API with its schema, a local data source, a
/// pipeline function, a resolver and an API key, with every output of
/// [`DECLARED_OUTPUTS`].
///
/// `api_name`, `schema`, `source_name`, `code` and `description` are what the two revisions
/// differ in: the first four are properties the owning update calls take, and the data
/// source's NAME is the one the replacement case moves.
fn template(
    api_name: &str,
    schema: &str,
    source_name: &str,
    code: &str,
    description: &str,
    xray: bool,
) -> String {
    let outputs: Vec<String> = DECLARED_OUTPUTS
        .iter()
        .map(|(key, value)| format!(r#""{key}":{{"Value":{value}}}"#))
        .collect();
    let schema = schema.replace('"', "\\\"");
    let code = code.replace('"', "\\\"");
    format!(
        concat!(
            r#"{{"Resources":{{"#,
            r#""Api":{{"Type":"AWS::AppSync::GraphQLApi","Properties":{{"#,
            r#""Name":"{api_name}","AuthenticationType":"API_KEY","Visibility":"GLOBAL","#,
            r#""XrayEnabled":{xray}}}}},"#,
            r#""Schema":{{"Type":"AWS::AppSync::GraphQLSchema","Properties":{{"#,
            r#""ApiId":{{"Fn::GetAtt":["Api","ApiId"]}},"Definition":"{schema}"}}}},"#,
            r#""Source":{{"Type":"AWS::AppSync::DataSource","UpdateReplacePolicy":"Retain","#,
            r#""Properties":{{"ApiId":{{"Fn::GetAtt":["Api","ApiId"]}},"#,
            r#""Name":"{source_name}","Type":"NONE","Description":"{description}"}}}},"#,
            r#""Enrich":{{"Type":"AWS::AppSync::FunctionConfiguration","#,
            r#""DependsOn":["Schema","Source"],"Properties":{{"#,
            r#""ApiId":{{"Fn::GetAtt":["Api","ApiId"]}},"Name":"Enrich","#,
            r#""DataSourceName":{{"Fn::GetAtt":["Source","Name"]}},"#,
            r#""Description":"{description}","Code":"{code}","#,
            r#""Runtime":{{"Name":"APPSYNC_JS","RuntimeVersion":"1.0.0"}}}}}},"#,
            r#""Resolve":{{"Type":"AWS::AppSync::Resolver","#,
            r#""DependsOn":["Schema","Source"],"Properties":{{"#,
            r#""ApiId":{{"Fn::GetAtt":["Api","ApiId"]}},"#,
            r#""TypeName":"Query","FieldName":"note","Kind":"UNIT","#,
            r#""DataSourceName":{{"Fn::GetAtt":["Source","Name"]}},"#,
            r#""Code":"{code}","#,
            r#""Runtime":{{"Name":"APPSYNC_JS","RuntimeVersion":"1.0.0"}}}}}},"#,
            r#""Key":{{"Type":"AWS::AppSync::ApiKey","Properties":{{"#,
            r#""ApiId":{{"Fn::GetAtt":["Api","ApiId"]}},"Description":"{description}"}}}}}},"#,
            r#""Outputs":{{{outputs}}}}}"#,
        ),
        api_name = api_name,
        xray = xray,
        schema = schema,
        source_name = source_name,
        description = description,
        code = code,
        outputs = outputs.join(","),
    )
}

/// Rules 1 and 2: every declared output comes back with both halves, resolved, and the
/// identity each type publishes is the handle AppSync knows the resource by.
#[tokio::test]
async fn every_output_a_deployed_appsync_template_declares_carries_its_value() {
    let router = build_router();
    let created = template("notes-api", SCHEMA, SOURCE_NAME, RESOLVER_CODE, "first", true);
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &created)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, "CREATE_COMPLETE").await;

    // RULE 1, read off the list itself: every member of `Outputs` carries BOTH members, and
    // the keys are exactly the ones the template declared.
    let case = "a deployed AppSync estate";
    let listed = block_of(&described, "Outputs")
        .unwrap_or_else(|| panic!("case {case:?}: expected an Outputs list, got {described}"));
    let rows = members_of(listed);
    assert_eq!(
        rows.len(),
        DECLARED_OUTPUTS.len(),
        "case {case:?}: expected one output per declared output, got {} in {described}",
        rows.len()
    );
    for row in &rows {
        let key = elements(row, "OutputKey").into_iter().next();
        let value = elements(row, "OutputValue").into_iter().next();
        assert!(
            key.is_some() && value.is_some(),
            "case {case:?}: expected every output to carry both members, got the row {row:?}"
        );
    }
    let outputs = outputs_of(&described);
    for (key, intrinsic) in DECLARED_OUTPUTS {
        let value = output(&outputs, key, case);
        assert!(
            !value.is_empty(),
            "case {case:?}: expected the output {key:?} ({intrinsic}) to carry a resolved \
             value, got {value:?}"
        );
        assert!(
            !value.contains("Fn::") && !value.contains("Ref"),
            "case {case:?}: expected the intrinsic behind {key:?} to be resolved, got {value:?}"
        );
    }

    // RULE 2: what AppSync itself reports for the API is what the stack published for it.
    let api_id = output(&outputs, "ApiIdOut", case);
    let (status, read) = appsync(&router, &format!("/v1/apis/{api_id}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetGraphqlApi on the published id \
         ({api_id:?}), got {status} with body {read}"
    );
    let api = &read["graphqlApi"];
    let api_arn = api["arn"].as_str().unwrap_or_default().to_string();
    let physical = physical_ids_of(&stack_resources(&router).await);

    // (case, the output, the value it must carry, the logical id whose physical id it is)
    let key_id = output(&outputs, "KeyId", case);
    let function_id = output(&outputs, "FunctionId", case);
    let identities: &[(&str, &str, String, Option<&str>)] = &[
        (
            "the API's Ref is the ARN AppSync built",
            "ApiRef",
            api_arn.clone(),
            Some("Api"),
        ),
        (
            "the API's Arn attribute is that same ARN",
            "ApiArn",
            api_arn.clone(),
            None,
        ),
        (
            "the schema is the API's id with the type's own segment appended",
            "SchemaRef",
            format!("{api_id}GraphQLSchema"),
            Some("Schema"),
        ),
        (
            "a data source's Ref is its ARN under the API",
            "SourceRef",
            format!("{api_arn}/datasources/{SOURCE_NAME}"),
            Some("Source"),
        ),
        (
            "and its DataSourceArn attribute is the same ARN",
            "SourceArn",
            format!("{api_arn}/datasources/{SOURCE_NAME}"),
            None,
        ),
        (
            "a function's Ref is its ARN under the API, built on the id AppSync minted",
            "FunctionRef",
            format!("{api_arn}/functions/{function_id}"),
            Some("Enrich"),
        ),
        (
            "a resolver's Ref is its ARN under the type and field it is attached to",
            "ResolverRef",
            format!("{api_arn}/types/Query/resolvers/note"),
            Some("Resolve"),
        ),
        (
            "an API key's Ref is its ARN under the API, never the key itself",
            "KeyRef",
            format!("{api_arn}/apikeys/{key_id}"),
            Some("Key"),
        ),
        (
            "and its Arn attribute is that same ARN",
            "KeyArn",
            format!("{api_arn}/apikeys/{key_id}"),
            None,
        ),
        (
            "the key a caller presents is the key's own id",
            "KeyValue",
            key_id.clone(),
            None,
        ),
    ];
    for (row, key, expected, logical_id) in identities {
        let published = output(&outputs, key, row);
        assert_eq!(
            &published, expected,
            "case {row:?}: expected the output {key:?} to be {expected:?}, got {published:?}"
        );
        if let Some(logical_id) = logical_id {
            let reported = physical.get(*logical_id).cloned().unwrap_or_default();
            assert_eq!(
                &reported, expected,
                "case {row:?}: expected the physical id of {logical_id:?} to be \
                 {expected:?}, got {reported:?}"
            );
        }
    }

    // The endpoints and the declared values the stack published are the ones AppSync
    // answered — never a value CloudFormation invented.
    let answered: &[(&str, &str, String)] = &[
        (
            "the GraphQL endpoint's host",
            "ApiGraphqlDns",
            api["dns"]["GRAPHQL"].as_str().unwrap_or_default().to_string(),
        ),
        (
            "the GraphQL endpoint's URL",
            "ApiGraphqlUrl",
            api["uris"]["GRAPHQL"].as_str().unwrap_or_default().to_string(),
        ),
        (
            "the real-time endpoint's host",
            "ApiRealtimeDns",
            api["dns"]["REALTIME"].as_str().unwrap_or_default().to_string(),
        ),
        (
            "the real-time endpoint's URL",
            "ApiRealtimeUrl",
            api["uris"]["REALTIME"].as_str().unwrap_or_default().to_string(),
        ),
        (
            "the data source's name",
            "SourceName",
            SOURCE_NAME.to_string(),
        ),
        (
            "the function's name",
            "FunctionName",
            "Enrich".to_string(),
        ),
        (
            "the data source the function runs against",
            "FunctionSource",
            SOURCE_NAME.to_string(),
        ),
        (
            "the type the resolver is attached to",
            "ResolverType",
            "Query".to_string(),
        ),
        (
            "the field it resolves",
            "ResolverField",
            "note".to_string(),
        ),
    ];
    for (row, key, expected) in answered {
        assert!(
            !expected.is_empty(),
            "case {row:?}: expected AppSync to report a value for {key:?}, got {read}"
        );
        let published = output(&outputs, key, row);
        assert_eq!(
            &published, expected,
            "case {row:?}: expected the output {key:?} to be {expected:?}, got {published:?}"
        );
    }

    // ...and the schema the stack installed is the one that API really serves, which is what
    // makes the published endpoint answer anything at all.
    let (status, served) =
        appsync_text(&router, &format!("/v1/apis/{api_id}/schema?format=SDL")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetIntrospectionSchema, got {status} with \
         {served}"
    );
    assert!(
        served.contains("type Note"),
        "case {case:?}: expected the installed schema to be served, got {served}"
    );
}

/// Rule 3: a changed declaration of a property the owning update call takes is applied in
/// place, and a change to a value the resource's identity is built from replaces it.
#[tokio::test]
async fn a_changed_appsync_declaration_is_applied_in_place() {
    let router = build_router();
    let created = template("notes-api", SCHEMA, SOURCE_NAME, RESOLVER_CODE, "first", true);
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &created)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, "CREATE_COMPLETE").await;
    let before = physical_ids_of(&stack_resources(&router).await);
    let api_id = output(&outputs_of(&described), "ApiIdOut", "the created stack");

    // Every property changed here is a member of that type's own update call, so none of
    // them is a change that recreates the resource.
    let changed = template(
        "notes-api-renamed",
        WIDER_SCHEMA,
        SOURCE_NAME,
        WIDER_RESOLVER_CODE,
        "second",
        false,
    );
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", STACK), ("TemplateBody", &changed)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, "UPDATE_COMPLETE").await;

    // The resource is the same one: every physical id is the one the create published.
    let after = physical_ids_of(&stack_resources(&router).await);
    for logical_id in ["Api", "Schema", "Source", "Enrich", "Resolve", "Key"] {
        let kept = after.get(logical_id).cloned().unwrap_or_default();
        let had = before.get(logical_id).cloned().unwrap_or_default();
        assert_eq!(
            kept, had,
            "case {logical_id:?}: expected the in-place update to keep the physical id \
             {had:?}, got {kept:?}"
        );
    }

    // ...and AppSync reports the configuration the changed template declared, read back
    // through that service's own reads: (case, the path read, the member, the new value).
    let function_id = after
        .get("Enrich")
        .and_then(|arn| arn.rsplit('/').next())
        .unwrap_or_default()
        .to_string();
    let key_id = after
        .get("Key")
        .and_then(|arn| arn.rsplit('/').next())
        .unwrap_or_default()
        .to_string();
    let reads: &[(&str, String, &[&str], &str)] = &[
        (
            "the API's name",
            format!("/v1/apis/{api_id}"),
            &["graphqlApi", "name"],
            "notes-api-renamed",
        ),
        (
            // A member the update call does not take is not lost by the change: the API is
            // still reachable from where its create said.
            "the API's visibility, which no update call takes",
            format!("/v1/apis/{api_id}"),
            &["graphqlApi", "visibility"],
            "GLOBAL",
        ),
        (
            "the data source's description",
            format!("/v1/apis/{api_id}/datasources/{SOURCE_NAME}"),
            &["dataSource", "description"],
            "second",
        ),
        (
            "the function's description",
            format!("/v1/apis/{api_id}/functions/{function_id}"),
            &["functionConfiguration", "description"],
            "second",
        ),
        (
            "the resolver's code",
            format!("/v1/apis/{api_id}/types/Query/resolvers/note"),
            &["resolver", "code"],
            WIDER_RESOLVER_CODE,
        ),
    ];
    for (case, path, member, expected) in reads {
        let (status, read) = appsync(&router, path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read of {path:?}, got {status} with \
             {read}"
        );
        let mut value = &read;
        for name in *member {
            value = &value[*name];
        }
        assert_eq!(
            value.as_str(),
            Some(*expected),
            "case {case:?}: expected AppSync to report {expected:?}, got {read}"
        );
    }
    // The API key's description is changed by the same rule; AppSync reports a key through
    // the listing of the API's keys, which is the only read it publishes for one.
    let (status, keys) = appsync(&router, &format!("/v1/apis/{api_id}/apikeys")).await;
    assert_eq!(
        status, 200,
        "case \"the key's description\": expected HTTP 200 from ListApiKeys, got {status} \
         with {keys}"
    );
    let description = keys["apiKeys"]
        .as_array()
        .and_then(|listed| {
            listed
                .iter()
                .find(|key| key["id"].as_str() == Some(key_id.as_str()))
        })
        .map(|key| key["description"].clone())
        .unwrap_or(Value::Null);
    assert_eq!(
        description.as_str(),
        Some("second"),
        "case \"the key's description\": expected the changed description, got {keys}"
    );

    // ...and the wider schema is the one the API now serves.
    let (status, served) =
        appsync_text(&router, &format!("/v1/apis/{api_id}/schema?format=SDL")).await;
    assert_eq!(
        status, 200,
        "case \"the installed schema\": expected HTTP 200 from GetIntrospectionSchema, got \
         {status} with {served}"
    );
    assert!(
        served.contains("author"),
        "case \"the installed schema\": expected the changed definition to be served, got \
         {served}"
    );
}

/// Rule 3, the other half: a change to a value the resource's identity is built from
/// REPLACES it, and `UpdateReplacePolicy: Retain` leaves the replaced resource where it is.
#[tokio::test]
async fn moving_an_appsync_resource_s_identity_replaces_it() {
    let router = build_router();
    let created = template("notes-api", SCHEMA, SOURCE_NAME, RESOLVER_CODE, "first", true);
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &created)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, "CREATE_COMPLETE").await;
    let outputs = outputs_of(&described);
    let api_id = output(&outputs, "ApiIdOut", "the created stack");
    let before = output(&outputs, "SourceRef", "the created stack");

    // A data source is addressed BY its name — `UpdateDataSource` takes no new one — so a
    // declaration naming another data source asks for a different resource.
    let moved = template(
        "notes-api",
        SCHEMA,
        REPLACED_SOURCE_NAME,
        RESOLVER_CODE,
        "first",
        true,
    );
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", STACK), ("TemplateBody", &moved)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, "UPDATE_COMPLETE").await;
    let outputs = outputs_of(&described);
    let case = "a data source whose name moved";

    let after = output(&outputs, "SourceRef", case);
    assert_ne!(
        after, before,
        "case {case:?}: expected the replacement to publish another identity, got {after:?}"
    );
    assert_eq!(
        after,
        output(&outputs, "SourceArn", case),
        "case {case:?}: expected the Ref and the ARN attribute to be one value, got {after:?}"
    );
    assert!(
        after.ends_with(&format!("/datasources/{REPLACED_SOURCE_NAME}")),
        "case {case:?}: expected the new identity to name the data source the template \
         declares, got {after:?}"
    );

    // The replacement exists in AppSync, and the one it replaced was RETAINED: the policy
    // "keeps the resource without deleting" it.
    for (row, name) in [
        ("the replacement", REPLACED_SOURCE_NAME),
        ("the retained data source", SOURCE_NAME),
    ] {
        let (status, read) =
            appsync(&router, &format!("/v1/apis/{api_id}/datasources/{name}")).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected AppSync to hold the data source {name:?}, got {status} \
             with {read}"
        );
        assert_eq!(
            read["dataSource"]["name"].as_str(),
            Some(name),
            "case {row:?}: expected the read to report {name:?}, got {read}"
        );
    }
}

/// `DescribeStackResources` for the suite's stack, as the XML answer.
async fn stack_resources(router: &MultiServiceRouter) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", STACK)]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeStackResources, got {status} with body {body}"
    );
    // The answer must carry the stack's own id, so a suite reading it is reading this stack.
    let _ = element(&body, "StackName");
    body
}
