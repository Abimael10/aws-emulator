// SPDX-License-Identifier: CC0-1.0

//! An in-place update of `AWS::ApiGateway::RestApi`: a stack update that states a different
//! value for a property of an api API Gateway already holds CHANGES THAT API.
//!
//! RULE: a property of a declaration whose owning service publishes a way to write it on the
//! resource that EXISTS is an in-place update — CloudFormation applies it through that call
//! and the logical resource keeps the physical resource it had — and only a property that
//! service fixes when the resource is created makes the update a replacement. For a REST API
//! that call is `UpdateRestApi` (`PATCH /restapis/{restApiId}`), whose `patchOperations` write
//! the api's name, description, api-key source, compression threshold, execute-api flag,
//! resource policy, binary media types and endpoint configuration; its tags are that service's
//! own tagging calls. So an update stating any of those settles `UPDATE_COMPLETE`, the api's
//! own id and root resource id are the ones it already had, the method attached to it is left
//! as it is, and API Gateway's own read reports the value the new template states.
//!
//! The api's NAME is one of them: an api is identified by the id API Gateway minted for it
//! (`Ref` returns that id and every call on an api takes it), so what the declaration calls
//! the api is only its name — `UpdateRestApi` writes it on `/name` — and renaming one is a
//! modification of the api rather than a new api.
//!
//! The members that call writes are read from the model rather than listed from memory —
//! `smithy_query(query="operation",operation="UpdateRestApi",model_path="services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json")`,
//! whose `RestApi` output carries them beside the ones that service assigns itself (`id`,
//! `createdDate`, `rootResourceId`, `warnings`, `apiStatus`), which no declaration states.
//!
//! The rows of the table differ along what the rule generalises over: WHICH property changed
//! and the SHAPE of the change — a scalar replaced, a scalar stated for the first time, one
//! dropped, an enum-valued member, a boolean flipped, an entry added to a list, an entry taken
//! out of it, a document the owning service normalises, and a tag. Every expected value is the
//! one this file's template states, read back through API Gateway's own wire rather than out
//! of the stack's records.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::harness::{call, member, router, settled_stack, xml_text, xml_texts, REGION};

/// The stack statuses this suite holds its deployments to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// What API Gateway's own read of the api must report for the property a row changed.
enum Expected {
    /// The member reports exactly this value.
    Exactly(Value),
    /// The api no longer reports the member at all, which is what the patch that CLEARS a
    /// pointer leaves behind.
    Absent,
    /// The member's text CARRIES this: a resource policy is a document API Gateway
    /// normalises (it expands the resources a statement names), so what the change is read by
    /// is the part of the document the template wrote.
    Carries(&'static str),
    /// The member reports exactly these entries, in whatever order that service lists a
    /// collection in.
    Entries(&'static [&'static str]),
    /// The member is a map the caller keys, and this key reports this value.
    Entry(&'static str, &'static str),
}

/// One row: the stack it deploys, the properties the api is first declared with beside the
/// base ones, the properties the update states beside them, and what API Gateway must report
/// for the member that carries the change.
struct Row {
    case: &'static str,
    stack: String,
    before: Value,
    after: Value,
    reported: (&'static str, Expected),
}

/// The base properties every row's api is declared with: a name of its own and a regional
/// endpoint, neither of which any row changes except the one that is about the name.
fn api_properties(stack: &str, stated: &Value) -> Value {
    let mut properties = Map::new();
    properties.insert("Name".to_string(), json!(format!("{stack}-api")));
    properties.insert(
        "EndpointConfiguration".to_string(),
        json!({ "Types": ["REGIONAL"] }),
    );
    for (property, value) in stated.as_object().expect("the row states an object") {
        properties.insert(property.clone(), value.clone());
    }
    Value::Object(properties)
}

/// The template a row deploys: the api, and one method on its ROOT resource with a mock
/// integration — the resource attached to the api, which an in-place change of the api must
/// leave exactly as it is.
fn template(stack: &str, stated: &Value) -> String {
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": api_properties(stack, stated)
            },
            "RootGet": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "HttpMethod": "GET",
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": mock_integration(r#"{"greeting":"hello"}"#)
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "RootResourceId": { "Value": { "Fn::GetAtt": ["Api", "RootResourceId"] } }
        }
    })
    .to_string()
}

/// A MOCK integration answering 200 with `body`: a mock has no backend, so the `statusCode`
/// its rendered request template names is the integration response the call maps to.
fn mock_integration(body: &str) -> Value {
    json!({
        "Type": "MOCK",
        "RequestTemplates": { "application/json": r#"{"statusCode":200}"# },
        "IntegrationResponses": [{
            "StatusCode": "200",
            "ResponseTemplates": { "application/json": body }
        }]
    })
}

/// The template the composite case deploys: the api with `description`, and the method whose
/// mock answers `greeting`. Both halves are stated here, so one update can restate both.
fn mock_api_template(stack: &str, description: &str, greeting: &str) -> String {
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "Description": description,
                    "EndpointConfiguration": { "Types": ["REGIONAL"] },
                    "DisableExecuteApiEndpoint": true
                }
            },
            "RootGet": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "HttpMethod": "GET",
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": mock_integration(greeting)
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "RootResourceId": { "Value": { "Fn::GetAtt": ["Api", "RootResourceId"] } }
        }
    })
    .to_string()
}

/// One resource policy of the api, under the `Sid` the row states — the part of the document
/// that says WHICH declaration the api is carrying.
fn api_policy(statement_id: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": statement_id,
            "Effect": "Allow",
            "Principal": "*",
            "Action": "execute-api:Invoke",
            "Resource": "execute-api:/*/GET/"
        }]
    })
}

/// Every row of the table, each under a stack of its own so one row's api cannot be read for
/// another's.
fn rows() -> Vec<Row> {
    let specifications: Vec<(&'static str, Value, Value, (&'static str, Expected))> = vec![
        (
            "a replaced description — a scalar the patch call writes",
            json!({ "Description": "as first declared" }),
            json!({ "Description": "as redeclared" }),
            ("description", Expected::Exactly(json!("as redeclared"))),
        ),
        (
            "a dropped description — the pointer's own remove",
            json!({ "Description": "as first declared" }),
            json!({}),
            ("description", Expected::Absent),
        ),
        (
            "a renamed api — the name is what the api is called, not what it is",
            json!({ "Name": "in-place-rename-first" }),
            json!({ "Name": "in-place-rename-second" }),
            ("name", Expected::Exactly(json!("in-place-rename-second"))),
        ),
        (
            "a compression threshold stated for the first time",
            json!({}),
            json!({ "MinimumCompressionSize": 10_240 }),
            (
                "minimumCompressionSize",
                Expected::Exactly(json!(10_240)),
            ),
        ),
        (
            "a compression threshold dropped",
            json!({ "MinimumCompressionSize": 10_240 }),
            json!({}),
            ("minimumCompressionSize", Expected::Absent),
        ),
        (
            "a different api-key source — an enum-valued member",
            json!({ "ApiKeySourceType": "HEADER" }),
            json!({ "ApiKeySourceType": "AUTHORIZER" }),
            ("apiKeySource", Expected::Exactly(json!("AUTHORIZER"))),
        ),
        (
            "the execute-api endpoint turned back on — a boolean flipped",
            json!({ "DisableExecuteApiEndpoint": true }),
            json!({ "DisableExecuteApiEndpoint": false }),
            (
                "disableExecuteApiEndpoint",
                Expected::Exactly(json!(false)),
            ),
        ),
        (
            "a binary media type added — one entry of a list",
            json!({ "BinaryMediaTypes": ["image/png"] }),
            json!({ "BinaryMediaTypes": ["image/png", "application/pdf"] }),
            (
                "binaryMediaTypes",
                Expected::Entries(&["application/pdf", "image/png"]),
            ),
        ),
        (
            "a binary media type taken out of that list",
            json!({ "BinaryMediaTypes": ["image/png", "application/pdf"] }),
            json!({ "BinaryMediaTypes": ["application/pdf"] }),
            (
                "binaryMediaTypes",
                Expected::Entries(&["application/pdf"]),
            ),
        ),
        (
            "a replaced resource policy — a document that service normalises",
            json!({ "Policy": api_policy("asFirstDeclared") }),
            json!({ "Policy": api_policy("asRedeclared") }),
            ("policy", Expected::Carries("asRedeclared")),
        ),
        (
            "a changed tag value — the api's own tagging calls",
            json!({ "Tags": [{ "Key": "owner", "Value": "first" }] }),
            json!({ "Tags": [{ "Key": "owner", "Value": "second" }] }),
            ("tags", Expected::Entry("owner", "second")),
        ),
    ];
    specifications
        .into_iter()
        .enumerate()
        .map(|(index, (case, before, after, reported))| Row {
            case,
            stack: format!("rest-api-in-place-{}", index + 1),
            before,
            after,
            reported,
        })
        .collect()
}

/// Deploy `body` under `stack` with `action`, wait for the stack to settle and hold it to
/// `wanted`, answering the `DescribeStacks` body that reported it.
///
/// The wait is on the CONDITION the shared waiter polls for — a status no operation is still
/// in flight in — so nothing here waits for a span.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    body: &str,
) -> String {
    let (code, answered) = call(
        router,
        action,
        &[member("StackName", stack), member("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected {action} to answer HTTP 200, got {code} with body {answered}"
    );
    let described = settled_stack(case, router, REGION, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    if settled != wanted_status(action) {
        let (_, events) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
        panic!(
            "case {case:?}: expected {action} to reach {}, got {settled:?} with resource \
             failures {:?}",
            wanted_status(action),
            xml_texts(&events, "ResourceStatusReason")
        );
    }
    described
}

/// The status the stack is owed once the named operation has gone through.
fn wanted_status(action: &str) -> &'static str {
    match action {
        "CreateStack" => CREATE_COMPLETE,
        _ => UPDATE_COMPLETE,
    }
}

/// The value of the stack output `name`, or a panic naming what the stack published instead.
fn output(described: &str, name: &str, case: &str) -> String {
    let values: Vec<(String, String)> = xml_texts(described, "OutputKey")
        .into_iter()
        .zip(xml_texts(described, "OutputValue"))
        .collect();
    values
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no {name} output: {described}"))
}

/// The physical id the stack records for `logical_id`.
async fn physical_id(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    case: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResources to answer HTTP 200, got {code} with \
         body {body}"
    );
    xml_texts(&body, "LogicalResourceId")
        .into_iter()
        .zip(xml_texts(&body, "PhysicalResourceId"))
        .find(|(logical, _)| logical == logical_id)
        .map(|(_, physical)| physical)
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for {logical_id} in {body}"))
}

/// Hold API Gateway's own read of the api to what the row expects of the member that carries
/// its change.
fn assert_reported(case: &str, reported: &Value, member_name: &str, expected: &Expected) {
    let answered = reported.get(member_name);
    match expected {
        Expected::Exactly(value) => assert_eq!(
            answered,
            Some(value),
            "case {case:?}: expected {member_name} to report {value}, got {reported}"
        ),
        Expected::Absent => assert!(
            answered.is_none(),
            "case {case:?}: expected the api to report no {member_name}, got {reported}"
        ),
        Expected::Carries(fragment) => assert!(
            answered
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(fragment)),
            "case {case:?}: expected {member_name} to carry {fragment:?}, got {reported}"
        ),
        Expected::Entries(wanted) => {
            let mut listed: Vec<String> = answered
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            listed.sort();
            let mut expected: Vec<String> = wanted.iter().map(ToString::to_string).collect();
            expected.sort();
            assert_eq!(
                listed, expected,
                "case {case:?}: expected {member_name} to report {expected:?}, got {reported}"
            );
        }
        Expected::Entry(key, value) => assert_eq!(
            answered.and_then(|entries| entries.get(*key)).and_then(Value::as_str),
            Some(*value),
            "case {case:?}: expected {member_name}[{key}] to report {value:?}, got {reported}"
        ),
    }
}

/// Every property of a REST API that API Gateway publishes a way to write on an api that
/// exists is applied to THAT api: the stack reaches `UPDATE_COMPLETE`, keeps the api's id,
/// its root resource id and the method on it, and that service reports the new value.
#[tokio::test]
async fn every_writable_property_of_a_rest_api_is_applied_to_the_api_that_exists() {
    let router = router();
    for row in rows() {
        let Row {
            case,
            stack,
            before,
            after,
            reported: (member_name, expected),
        } = row;

        let described = deploy(
            &router,
            case,
            "CreateStack",
            &stack,
            &template(&stack, &before),
        )
        .await;
        let api_id = output(&described, "ApiId", case);
        let root_id = output(&described, "RootResourceId", case);
        let method_before = physical_id(&router, &stack, "RootGet", case).await;

        let described = deploy(
            &router,
            case,
            "UpdateStack",
            &stack,
            &template(&stack, &after),
        )
        .await;

        // The api is the SAME api: an in-place change keeps the physical resource, so the id
        // the stack publishes and the root resource the methods hang off are the ones the
        // create answered.
        assert_eq!(
            output(&described, "ApiId", case),
            api_id,
            "case {case:?}: expected the update to keep the api ({api_id}), got {described}"
        );
        assert_eq!(
            output(&described, "RootResourceId", case),
            root_id,
            "case {case:?}: expected the update to keep the root resource ({root_id}), got \
             {described}"
        );

        // ...and the method attached to it is untouched: the stack records the same resource
        // and API Gateway still holds it on the api's root.
        assert_eq!(
            physical_id(&router, &stack, "RootGet", case).await,
            method_before,
            "case {case:?}: expected the method to be left as it is ({method_before})"
        );
        let (code, method) = apigateway(
            &router,
            "GET",
            &format!("/restapis/{api_id}/resources/{root_id}/methods/GET"),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the api to still hold its GET method, got {code} with \
             body {method}"
        );

        // The change itself, as the service that owns the api reports it.
        let (code, api) = apigateway(&router, "GET", &format!("/restapis/{api_id}")).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected GetRestApi to answer HTTP 200, got {code} with body {api}"
        );
        assert_reported(case, &api, member_name, &expected);
    }
}

/// The update a template-managed api takes with its methods: one `UpdateStack` restating the
/// api's description AND the body its mock integration answers settles `UPDATE_COMPLETE`, the
/// api keeps its id, and both the new description and the new mock body are what API Gateway
/// reports.
#[tokio::test]
async fn an_api_and_the_mock_it_answers_with_are_both_updated_in_place() {
    let router = router();
    let case = "an api redescribed while its mock answers something else";
    let stack = "rest-api-in-place-mock";

    let first = mock_api_template(stack, "as first declared", r#"{"greeting":"hello"}"#);
    let described = deploy(&router, case, "CreateStack", stack, &first).await;
    let api_id = output(&described, "ApiId", case);
    let root_id = output(&described, "RootResourceId", case);

    let second = mock_api_template(stack, "as redeclared", r#"{"greeting":"bonjour"}"#);
    let described = deploy(&router, case, "UpdateStack", stack, &second).await;
    assert_eq!(
        output(&described, "ApiId", case),
        api_id,
        "case {case:?}: expected the update to keep the api ({api_id}), got {described}"
    );

    let (code, api) = apigateway(&router, "GET", &format!("/restapis/{api_id}")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetRestApi to answer HTTP 200, got {code} with body {api}"
    );
    assert_eq!(
        api.get("description").and_then(Value::as_str),
        Some("as redeclared"),
        "case {case:?}: expected the api to report the new description, got {api}"
    );

    let (code, response) = apigateway(
        &router,
        "GET",
        &format!("/restapis/{api_id}/resources/{root_id}/methods/GET/integration/responses/200"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the integration response to be readable, got {code} with body \
         {response}"
    );
    assert_eq!(
        response
            .get("responseTemplates")
            .and_then(|templates| templates.get("application/json"))
            .and_then(Value::as_str),
        Some(r#"{"greeting":"bonjour"}"#),
        "case {case:?}: expected the mock to answer the new body, got {response}"
    );
}
