// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Bedrock::Flow` deploys: the flow is created
//! in the Amazon Bedrock Agent build-time plane that owns it, PREPARED there, published under
//! the identity that service minted — and the flow the stack publishes is one the Bedrock
//! Agent RUNTIME plane then invokes.
//!
//! The rule under test: **a resource type Amazon Web Services publishes is one a template may
//! declare, and a declaration of it is deployed by calling the service that OWNS the type, so
//! every handle the stack publishes resolves in that service and the resource is in the state
//! a client can use it in.** For a flow that means three things, each asserted below: the
//! template is admitted rather than refused `Template format error: Unrecognized resource
//! types`; `GetFlow` resolves the ARN the stack published and reports the declared name, role
//! and definition; and the flow is `Prepared`, which is the status an invocable flow is in —
//! `CreateFlow` alone leaves it `NotPrepared`, in which no invocation runs it. Three crates
//! have to agree for that to hold (CloudFormation submits the template and makes the calls,
//! the Bedrock Agent build-time plane serves them, IAM serves the execution role the flow
//! runs as), so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the template spelling (JSON with
//! long-form intrinsics, YAML with the short-form tags), whether the declaration NAMES the
//! flow or leaves the name to CloudFormation, whether it carries a description and the type's
//! own `Tags`, how the execution role is named (a literal ARN, `Fn::GetAtt` on a role the same
//! stack creates), and the SHAPE of the definition (one output node, two output nodes).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack and its flow
/// belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the flow's preparation is a transition the owning service's own clock
/// owes, and the suite waits for the CONDITION (the stack's status) rather than for a fixed
/// time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The signing name the whole Bedrock family shares; the route decides which member serves a
/// request.
const BEDROCK: &str = "bedrock";

/// The resource type under test, spelled as the "AWS resource and property types reference"
/// spells it.
const FLOW: &str = "AWS::Bedrock::Flow";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row here may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The `FlowStatus` an INVOCABLE flow is in: "Prepared – The flow is prepared and can be
/// invoked" (`smithy_query(query="shape",shape="FlowStatus")`).
const PREPARED: &str = "Prepared";

/// The version a flow a stack just created carries: `CreateFlow`'s own `version` is `^DRAFT$`.
const DRAFT: &str = "DRAFT";

/// An execution role ARN a declaration may state literally, inside
/// `FlowExecutionRoleArn`'s own pattern
/// (`^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`).
const LITERAL_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/cfn-flow-literal";

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
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
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one operation of a Bedrock-family plane over its restJson1 wire, answering the status,
/// the `content-type` and the raw body.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, String, Bytes) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{BEDROCK}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, content_type, response.into_body())
}

/// The same call, with the answer read as JSON.
async fn bedrock_json(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, _, raw) = bedrock(router, method, path, body).await;
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A `flowIdentifier` as a single path segment: a flow ARN carries `:` and `/`, which would
/// otherwise split the segment the label is bound to.
fn label(identifier: &str) -> String {
    identifier.replace('%', "%25").replace('/', "%2F")
}

/// `GetFlow` on one `flowIdentifier` (`GET /flows/{flowIdentifier}/`).
async fn get_flow(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    bedrock_json(
        router,
        "GET",
        &format!("/flows/{}/", label(identifier)),
        None,
    )
    .await
}

/// The `DescribeStacks` answer once the stack rests in a status that is not in progress — or
/// once it is GONE, which a delete leaves it — or the last one read when the deadline passes.
/// Waits for the CONDITION, looking again until the stack has settled.
async fn settled_stack(router: &MultiServiceRouter, stack_name: &str) -> (u16, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let settled = status != 200
            || opt_element(&body, "StackStatus")
                .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return (status, body);
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack and answer its settled `DescribeStacks` body, asserting the template was
/// ADMITTED: a type AWS publishes is never refused as an unrecognised one.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
    capabilities: &[&str],
) -> String {
    let mut members = vec![("StackName", stack_name), ("TemplateBody", template)];
    let declared: Vec<(String, String)> = capabilities
        .iter()
        .enumerate()
        .map(|(index, capability)| {
            (
                format!("Capabilities.member.{}", index + 1),
                (*capability).to_string(),
            )
        })
        .collect();
    for (key, value) in &declared {
        members.push((key.as_str(), value.as_str()));
    }
    let (status, body) = cloudformation(router, "CreateStack", &members).await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected {FLOW} to be a type a template may declare, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (status, body) = settled_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
    );
    body
}

/// A passthrough flow definition in the TEMPLATE's spelling — every property is the API member
/// of the same name with an upper-cased initial — with `outputs` output nodes fed from the one
/// input node. The node types, the configuration union keys and the IO data types are the
/// build-time model's own `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType`
/// values.
fn declared_definition(outputs: usize) -> Value {
    let mut nodes = vec![json!({
        "Name": "FlowInput",
        "Type": "Input",
        "Configuration": { "Input": {} },
        "Outputs": [{ "Name": "document", "Type": "String" }],
    })];
    let mut connections = Vec::new();
    for index in 0..outputs {
        let name = format!("FlowOutput{index}");
        nodes.push(json!({
            "Name": name,
            "Type": "Output",
            "Configuration": { "Output": {} },
            "Inputs": [{ "Name": "document", "Type": "String", "Expression": "$.data" }],
        }));
        connections.push(json!({
            "Name": format!("InputToOutput{index}"),
            "Type": "Data",
            "Source": "FlowInput",
            "Target": name,
            "Configuration": {
                "Data": { "SourceOutput": "document", "TargetInput": "document" },
            },
        }));
    }
    json!({ "Nodes": nodes, "Connections": connections })
}

/// What one row expects of the flow its stack declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    /// The capabilities the create acknowledges, which a template creating an IAM resource
    /// requires (`CAPABILITY_IAM`).
    capabilities: Vec<&'static str>,
    /// The name the declaration states, or `None` where it leaves the name to CloudFormation.
    declared_name: Option<&'static str>,
    /// How many output nodes the declared definition carries, which the stored flow must
    /// report back.
    output_nodes: usize,
}

fn rows() -> Vec<Row> {
    vec![
        // A named flow whose execution role is a literal ARN, with the type's own `Tags`, in
        // JSON with long-form intrinsics; the flow's ARN, id, status and version are all read
        // back through the stack's outputs.
        Row {
            case: "a named flow over a literal role, JSON spelling",
            stack_name: "bedrock-flow-named-stack",
            template: json!({
                "Resources": {
                    "Flow": {
                        "Type": FLOW,
                        "Properties": {
                            "Name": "cfn-flow-named",
                            "Description": "The flow this stack deploys.",
                            "ExecutionRoleArn": LITERAL_ROLE,
                            "Definition": declared_definition(1),
                            "Tags": { "team": "platform" },
                        },
                    },
                },
                "Outputs": {
                    "FlowRef": { "Value": { "Ref": "Flow" } },
                    "FlowArn": { "Value": { "Fn::GetAtt": ["Flow", "Arn"] } },
                    "FlowId": { "Value": { "Fn::GetAtt": ["Flow", "Id"] } },
                    "FlowStatus": { "Value": { "Fn::GetAtt": ["Flow", "Status"] } },
                    "FlowVersion": { "Value": { "Fn::GetAtt": ["Flow", "Version"] } },
                },
            })
            .to_string(),
            capabilities: Vec::new(),
            declared_name: Some("cfn-flow-named"),
            output_nodes: 1,
        },
        // The YAML spelling, the flow's name left to CloudFormation, the execution role read
        // off a role the SAME stack creates, and a definition fanning the input out to two
        // output nodes.
        Row {
            case: "an unnamed flow over a role the stack creates, two output nodes, YAML \
                   spelling",
            stack_name: "bedrock-flow-generated-stack",
            template: format!(
                "\
Resources:
  FlowRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: '2012-10-17'
        Statement:
          - Effect: Allow
            Principal:
              Service: bedrock.amazonaws.com
            Action: sts:AssumeRole
  Flow:
    Type: {FLOW}
    Properties:
      ExecutionRoleArn: !GetAtt FlowRole.Arn
      Definition: {definition}
Outputs:
  FlowRef:
    Value: !Ref Flow
  FlowId:
    Value: !GetAtt Flow.Id
  FlowStatus:
    Value: !GetAtt Flow.Status
  RoleArn:
    Value: !GetAtt FlowRole.Arn
",
                definition = serde_json::to_string(&declared_definition(2))
                    .expect("the definition serializes")
            ),
            capabilities: vec!["CAPABILITY_IAM"],
            declared_name: None,
            output_nodes: 2,
        },
    ]
}

/// One output of the settled stack, by key.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str, body: &str) -> String {
    outputs
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
        .clone()
}

/// Whether `id` has the form the owning service mints a `FlowId` in
/// (`smithy_query(query="operation",operation="CreateFlow")` -> `id` `^[0-9a-zA-Z]{10}$`).
fn is_a_minted_flow_id(id: &str) -> bool {
    id.len() == 10 && id.chars().all(|character| character.is_ascii_alphanumeric())
}

#[tokio::test]
async fn a_deployed_stack_publishes_a_flow_the_owning_service_holds_and_has_prepared() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(
            &router,
            case,
            row.stack_name,
            &row.template,
            &row.capabilities,
        )
        .await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let outputs = outputs_of(&body);

        // "Ref returns the flow ARN": the identity the owning service minted, which every
        // call on a flow is addressed by.
        let published = output(&outputs, case, "FlowRef", &body);
        let id = output(&outputs, case, "FlowId", &body);
        assert!(
            is_a_minted_flow_id(&id),
            "case {case:?}: expected the published Id to have the minted ^[0-9a-zA-Z]{{10}}$ \
             form, got {id:?}"
        );
        assert_eq!(
            published,
            format!("arn:aws:bedrock:{REGION}:000000000000:flow/{id}"),
            "case {case:?}: expected Ref to publish the ARN around the minted id, got \
             {published:?}"
        );

        // The handle the stack published is one the OWNING service resolves, and the flow it
        // resolves to is the one the template declared.
        let (status, reported) = get_flow(&router, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the build-time plane to resolve the published ARN \
             {published:?}, got {status} with {reported}"
        );
        assert_eq!(
            reported["id"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the flow to report the published id, got {reported}"
        );
        if let Some(name) = row.declared_name {
            assert_eq!(
                reported["name"].as_str(),
                Some(name),
                "case {case:?}: expected the flow to carry the declared name, got {reported}"
            );
        } else {
            let generated = reported["name"].as_str().unwrap_or_default();
            assert!(
                generated.starts_with(row.stack_name),
                "case {case:?}: expected a name CloudFormation generated for the stack, got \
                 {generated:?}"
            );
        }
        let role = reported["executionRoleArn"].as_str().unwrap_or_default();
        let declared_role = outputs
            .get("RoleArn")
            .cloned()
            .unwrap_or_else(|| LITERAL_ROLE.to_string());
        assert_eq!(
            role, declared_role,
            "case {case:?}: expected the flow to run as the declared role, got {reported}"
        );

        // The DEFINITION reached the owning service, re-cased into its own member names all
        // the way down — a flow created without it would run nothing.
        let nodes = reported["definition"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no definition nodes in {reported}"));
        assert_eq!(
            nodes.len(),
            row.output_nodes + 1,
            "case {case:?}: expected the declared nodes, got {reported}"
        );
        let input_node = nodes
            .iter()
            .find(|node| node["name"].as_str() == Some("FlowInput"))
            .unwrap_or_else(|| panic!("case {case:?}: no input node in {reported}"));
        assert_eq!(
            (
                input_node["type"].as_str(),
                input_node["configuration"].get("input").is_some(),
            ),
            (Some("Input"), true),
            "case {case:?}: expected the input node's own configuration member, got \
             {input_node}"
        );

        // And the flow is in the status an invocable flow is in, which is also what the
        // stack published for it.
        assert_eq!(
            reported["status"].as_str(),
            Some(PREPARED),
            "case {case:?}: expected the deployed flow to be {PREPARED}, got {reported}"
        );
        assert_eq!(
            output(&outputs, case, "FlowStatus", &body),
            PREPARED,
            "case {case:?}: expected the stack to publish the status the flow reports, got \
             {body}"
        );
        if outputs.contains_key("FlowVersion") {
            assert_eq!(
                output(&outputs, case, "FlowVersion", &body),
                DRAFT,
                "case {case:?}: expected the working version a created flow carries, got {body}"
            );
        }
    }
}

/// The deployed flow is REACHABLE from the runtime plane: an alias on it routes to the version
/// the stack prepared, and `InvokeFlow` on that alias runs the flow — which is the whole point
/// of deploying one through a stack.
#[tokio::test]
async fn the_flow_a_stack_deployed_is_invocable_through_the_runtime_plane() {
    let router = build_router();
    let case = "invoking the flow a stack deployed";
    let stack_name = "bedrock-flow-invoked-stack";
    let template = json!({
        "Resources": {
            "Flow": {
                "Type": FLOW,
                "Properties": {
                    "Name": "cfn-flow-invoked",
                    "ExecutionRoleArn": LITERAL_ROLE,
                    "Definition": declared_definition(1),
                },
            },
        },
        "Outputs": {
            "FlowRef": { "Value": { "Ref": "Flow" } },
            "FlowId": { "Value": { "Fn::GetAtt": ["Flow", "Id"] } },
        },
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template, &[]).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body}"
    );
    let outputs = outputs_of(&body);
    let flow_id = output(&outputs, case, "FlowId", &body);

    // An alias routing to the DRAFT the stack prepared, which is what an invocation addresses
    // a flow through (`InvokeFlow` takes a `flowAliasIdentifier`).
    let (status, alias) = bedrock_json(
        &router,
        "POST",
        &format!("/flows/{flow_id}/aliases/"),
        Some(json!({
            "name": "deployed",
            "routingConfiguration": [{ "flowVersion": DRAFT }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected CreateFlowAlias on the deployed flow to succeed, got {status} \
         with {alias}"
    );
    let alias_id = alias["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no alias id in {alias}"))
        .to_string();

    let (status, content_type, raw) = bedrock(
        &router,
        "POST",
        &format!("/flows/{flow_id}/aliases/{alias_id}"),
        Some(json!({
            "inputs": [{
                "nodeName": "FlowInput",
                "nodeOutputName": "document",
                "content": { "document": "the stack's own input" },
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the runtime plane to run the deployed flow, got {status} with \
         {:?}",
        String::from_utf8_lossy(&raw)
    );
    assert!(
        content_type.contains("vnd.amazon.eventstream"),
        "case {case:?}: expected an event-stream answer, got {content_type:?}"
    );
    // The frames of an event stream carry their `:event-type` header and their payload as
    // plain bytes, so what the flow delivered is readable in the body: the output node the
    // input reached, and the completion the stream ends on.
    let delivered = String::from_utf8_lossy(&raw).to_string();
    for expected in ["flowOutputEvent", "FlowOutput0", "the stack's own input"] {
        assert!(
            delivered.contains(expected),
            "case {case:?}: expected the stream to carry {expected:?}, got {delivered:?}"
        );
    }
}

/// Deleting the stack deletes the flow it created: the handle it published stops resolving in
/// the owning service, which is the other half of the rule.
#[tokio::test]
async fn deleting_the_stack_deletes_the_flow_it_created() {
    let router = build_router();
    let case = "a flow deleted with its stack";
    let stack_name = "bedrock-flow-deleted-stack";
    let template = json!({
        "Resources": {
            "Flow": {
                "Type": FLOW,
                "Properties": {
                    "Name": "cfn-flow-deleted",
                    "ExecutionRoleArn": LITERAL_ROLE,
                    "Definition": declared_definition(1),
                },
            },
        },
        "Outputs": { "FlowRef": { "Value": { "Ref": "Flow" } } },
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template, &[]).await;
    let outputs = outputs_of(&body);
    let published = output(&outputs, case, "FlowRef", &body);
    let (status, reported) = get_flow(&router, &published).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the flow to exist before the delete, got {status} with \
         {reported}"
    );

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    // The stack is gone once it no longer rests in an in-progress status — a deleted stack is
    // one `DescribeStacks` no longer resolves by name — and the flow it created is gone with
    // it.
    let (status, settled) = settled_stack(&router, stack_name).await;
    assert!(
        status != 200
            || opt_element(&settled, "StackStatus")
                .is_none_or(|stack_status| stack_status == "DELETE_COMPLETE"),
        "case {case:?}: expected the delete to complete, got {status} with {settled}"
    );
    let (status, gone) = get_flow(&router, &published).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the flow to be gone with its stack, got {status} with {gone}"
    );
}
