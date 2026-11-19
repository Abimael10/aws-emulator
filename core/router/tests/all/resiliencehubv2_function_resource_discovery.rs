// SPDX-License-Identifier: CC0-1.0

//! The resources `CreateServiceFunctionResources` associates with a Resilience Hub service
//! function are the ones the SERVICE discovered — as the component that owns them says.
//!
//! The rule under test: **a service's resources are discovered through its input sources, so
//! `CreateServiceFunctionResources` associates an identifier only where one of those sources
//! discovered it and refuses one no source did with the modelled 400 `ValidationException`,
//! writing nothing; the resources of a function are a set, so `resourceCount` is the number of
//! distinct identifiers associated. Where an input source names resources nothing here holds,
//! the service's resource set is unknown and no identifier is refused for being outside it.**
//!
//! Two crates have to agree for that to hold — Resilience Hub asks, CloudFormation answers
//! which resources a stack holds — so the suite lives here: a crate-local test can only ever
//! see one side of it.
//!
//! The rows differ along what the rule generalises over: the resource CLASS and so the FORM
//! its identifier takes (a queue, addressed by its url; a topic, addressed by its ARN), how
//! many identifiers one call names, whether the identifier was discovered at all, whether it
//! names a resource that exists but in a stack this service does not read, and whether an
//! identifier is named twice.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the stacks and their
/// resources belong to. It is the Region the shared stack wait polls in.
const REGION: &str = "us-east-1";

/// The stack status a deployed stack settles at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The modelled routes this suite drives.
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_INPUT_SOURCE: &str = "/v2/create-input-source";
const CREATE_SERVICE_FUNCTION: &str = "/v2/create-service-function";
const CREATE_SERVICE_FUNCTION_RESOURCES: &str = "/v2/create-service-function-resources";
const LIST_SERVICE_FUNCTIONS: &str = "/v2/list-functions";

/// The emulator as it is ASSEMBLED, on one engine: Resilience Hub asks CloudFormation, which
/// in turn needs the services owning the resources its templates declare.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is never checked.
    router.set_request_authentication(false);
    router
}

/// What a dispatched request answered: the status, the `x-amzn-errortype` code (empty on a
/// success) and the parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    /// The body, asserting the call succeeded.
    fn ok(&self, case: &str) -> &Value {
        assert_eq!(
            self.status, 200,
            "case {case:?}: expected HTTP 200, got {} {:?} {:?}",
            self.status, self.code, self.body
        );
        &self.body
    }

    /// The `message` the answer carried.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// One Resilience Hub v2 restJson1 call.
async fn resiliencehubv2(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body
        .map(|value| Bytes::from(value.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://resiliencehub.{REGION}.amazonaws.com{path}"))
        .header("host", format!("resiliencehub.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("resiliencehub"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer { status, code, body }
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
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Deploy `template` as the stack `name`, wait for it to complete, and answer its stack id
/// (which is the stack's ARN) with the logical-id -> physical-id map of what it holds.
async fn deploy(
    router: &MultiServiceRouter,
    name: &str,
    template: &Value,
) -> (String, std::collections::BTreeMap<String, String>) {
    let (status, created) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", name), ("TemplateBody", &template.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({name:?}) must be accepted, got {status} with {created}"
    );
    let stack_id = element(&created, "StackId");

    let (settled, answer) = wait_for_stack_status(router, name, CREATE_COMPLETE).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "setup: stack {name:?} must reach {CREATE_COMPLETE}, got {settled} with {answer}"
    );

    let (status, described) =
        cloudformation(router, "DescribeStackResources", &[("StackName", name)]).await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({name:?}) must be accepted, got {status} with {described}"
    );
    (stack_id, physical_ids_of(&described))
}

/// Create a service and answer its ARN.
async fn create_service(router: &MultiServiceRouter, name: &str) -> String {
    let created = resiliencehubv2(
        router,
        "POST",
        CREATE_SERVICE,
        Some(&json!({
            "name": name,
            "regions": [REGION],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        })),
    )
    .await;
    created.ok(name)["service"]["serviceArn"]
        .as_str()
        .expect("a minted serviceArn")
        .to_string()
}

/// Create an input source for `service_arn` from `configuration`.
async fn create_input_source(
    router: &MultiServiceRouter,
    service_arn: &str,
    configuration: Value,
) {
    resiliencehubv2(
        router,
        "POST",
        CREATE_INPUT_SOURCE,
        Some(&json!({"serviceArn": service_arn, "resourceConfiguration": configuration})),
    )
    .await
    .ok("the input source");
}

/// Create a service function and answer its id.
async fn create_function(router: &MultiServiceRouter, service_arn: &str, name: &str) -> String {
    let created = resiliencehubv2(
        router,
        "POST",
        CREATE_SERVICE_FUNCTION,
        Some(&json!({
            "serviceArn": service_arn,
            "name": name,
            "criticality": "PRIMARY",
        })),
    )
    .await;
    created.ok(name)["serviceFunction"]["serviceFunctionId"]
        .as_str()
        .expect("a minted serviceFunctionId")
        .to_string()
}

/// Associate `resources` with one service function.
async fn associate(
    router: &MultiServiceRouter,
    service_arn: &str,
    service_function_id: &str,
    resources: &[String],
) -> Answer {
    resiliencehubv2(
        router,
        "POST",
        CREATE_SERVICE_FUNCTION_RESOURCES,
        Some(&json!({
            "serviceArn": service_arn,
            "serviceFunctionId": service_function_id,
            "resources": resources,
        })),
    )
    .await
}

/// The `resourceCount` the service's own listing reports for one function: what a client
/// reads back after an association, and the only reading of what was stored.
async fn resource_count(
    router: &MultiServiceRouter,
    service_arn: &str,
    service_function_id: &str,
) -> i64 {
    let listed = resiliencehubv2(
        router,
        "GET",
        &format!("{LIST_SERVICE_FUNCTIONS}?serviceArn={service_arn}"),
        None,
    )
    .await;
    let body = listed.ok("the listing");
    let found = body["serviceFunctions"]
        .as_array()
        .expect("a serviceFunctions list")
        .iter()
        .find(|function| function["serviceFunctionId"] == json!(service_function_id))
        .unwrap_or_else(|| panic!("no function {service_function_id} in {body:?}"));
    found["resourceCount"]
        .as_i64()
        .unwrap_or_else(|| panic!("no resourceCount on {found:?}"))
}

/// The template the discovered resources come from: two resource classes, so the rows differ
/// in the FORM their identifiers take (a queue's url, a topic's ARN), and two resources of one
/// class, so a row can name more than one.
fn discovered_template() -> Value {
    json!({
        "Resources": {
            "Orders": {
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": "rhv2-discovered-orders"},
            },
            "Events": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": "rhv2-discovered-events"},
            },
            "Audit": {
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": "rhv2-discovered-audit"},
            },
        }
    })
}

/// One row of the association table.
struct Association {
    label: &'static str,
    /// The identifiers the call names.
    resources: Vec<String>,
    /// The identifier the refusal must name, or `None` where the call is served.
    undiscovered: Option<String>,
    /// How many distinct resources the function holds afterwards.
    expected_count: i64,
}

/// The whole table: an identifier one of the service's input sources discovered is
/// associated and counted once, and one no source discovered is the modelled 400 naming it,
/// with nothing stored.
#[tokio::test]
async fn only_resources_the_services_input_sources_discovered_are_associated() {
    let router = emulator();
    let (stack_arn, discovered) = deploy(&router, "rhv2-discovery", &discovered_template()).await;
    // A second stack, deployed but NOT an input source of the service: its resources exist,
    // and this service has still discovered none of them.
    let (_, elsewhere) = deploy(
        &router,
        "rhv2-elsewhere",
        &json!({
            "Resources": {
                "Other": {
                    "Type": "AWS::SQS::Queue",
                    "Properties": {"QueueName": "rhv2-elsewhere-other"},
                }
            }
        }),
    )
    .await;

    let queue = discovered
        .get("Orders")
        .expect("the stack reports the queue's url")
        .clone();
    let topic = discovered
        .get("Events")
        .expect("the stack reports the topic's ARN")
        .clone();
    let audit = discovered
        .get("Audit")
        .expect("the stack reports the second queue's url")
        .clone();
    let other_stacks_queue = elsewhere
        .get("Other")
        .expect("the other stack reports its queue's url")
        .clone();

    let service_arn = create_service(&router, "discovery-association").await;
    create_input_source(&router, &service_arn, json!({"cfnStackArn": stack_arn})).await;

    let rows = [
        Association {
            label: "a queue the stack discovered, by its url",
            resources: vec![queue.clone()],
            undiscovered: None,
            expected_count: 1,
        },
        Association {
            label: "a topic the stack discovered, by its ARN",
            resources: vec![topic.clone()],
            undiscovered: None,
            expected_count: 1,
        },
        Association {
            label: "three discovered resources in one call",
            resources: vec![queue.clone(), topic.clone(), audit.clone()],
            undiscovered: None,
            expected_count: 3,
        },
        Association {
            label: "one discovered resource named twice",
            resources: vec![queue.clone(), queue.clone()],
            undiscovered: None,
            expected_count: 1,
        },
        Association {
            label: "an ARN no input source discovered",
            resources: vec![format!("arn:aws:sqs:{REGION}:000000000000:rhv2-ghost-queue")],
            undiscovered: Some(format!("arn:aws:sqs:{REGION}:000000000000:rhv2-ghost-queue")),
            expected_count: 0,
        },
        Association {
            label: "a bare name no input source discovered",
            resources: vec!["rhv2-ghost".to_string()],
            undiscovered: Some("rhv2-ghost".to_string()),
            expected_count: 0,
        },
        Association {
            label: "a resource that exists, in a stack this service does not read",
            resources: vec![other_stacks_queue.clone()],
            undiscovered: Some(other_stacks_queue.clone()),
            expected_count: 0,
        },
        Association {
            label: "a discovered resource beside an undiscovered one",
            resources: vec![queue.clone(), "rhv2-ghost-too".to_string()],
            undiscovered: Some("rhv2-ghost-too".to_string()),
            expected_count: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.label;
        // One fresh function per row, so each row's count is its own call's.
        let function_id = create_function(&router, &service_arn, &format!("Function-{index}")).await;
        let answered = associate(&router, &service_arn, &function_id, &row.resources).await;

        match &row.undiscovered {
            None => {
                let body = answered.ok(case);
                assert_eq!(
                    body["serviceArn"].as_str(),
                    Some(service_arn.as_str()),
                    "case {case:?}: the answer must name the service the request did, got \
                     {body:?}"
                );
                assert_eq!(
                    body["serviceFunctionId"].as_str(),
                    Some(function_id.as_str()),
                    "case {case:?}: the answer must name the function the request did, got \
                     {body:?}"
                );
                let mut named: Vec<String> = Vec::new();
                for identifier in &row.resources {
                    if !named.contains(identifier) {
                        named.push(identifier.clone());
                    }
                }
                assert_eq!(
                    body["resources"],
                    json!(named),
                    "case {case:?}: the answer must report the resources it associated, each \
                     once, got {body:?}"
                );
            }
            Some(identifier) => {
                assert_eq!(
                    (answered.status, answered.code.as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: expected the modelled 400 ValidationException, got {} {:?} \
                     {:?}",
                    answered.status,
                    answered.code,
                    answered.body
                );
                let expected = format!("Resource not discovered for service: {identifier}");
                let actual = answered.message();
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {expected:?}, got {actual:?}"
                );
            }
        }

        let counted = resource_count(&router, &service_arn, &function_id).await;
        assert_eq!(
            counted, row.expected_count,
            "case {case:?}: expected the function to hold {} resources, got {counted}",
            row.expected_count
        );
    }
}

/// A second association adds to what the first stored, and re-associating a resource the
/// function already holds leaves the count where it was: the resources of a function are a
/// SET, and the listing is what a client reads them back through.
#[tokio::test]
async fn associating_again_adds_only_what_the_function_did_not_hold() {
    let router = emulator();
    let (stack_arn, discovered) =
        deploy(&router, "rhv2-repeat-discovery", &discovered_template()).await;
    let queue = discovered.get("Orders").expect("the queue's url").clone();
    let topic = discovered.get("Events").expect("the topic's ARN").clone();

    let service_arn = create_service(&router, "repeated-association").await;
    create_input_source(&router, &service_arn, json!({"cfnStackArn": stack_arn})).await;
    let function_id = create_function(&router, &service_arn, "Checkout").await;

    for (case, resources, expected_count) in [
        ("the first association", vec![queue.clone()], 1),
        ("the same resource again", vec![queue.clone()], 1),
        ("one resource more", vec![topic.clone()], 2),
        (
            "both, one of them already held",
            vec![queue.clone(), topic.clone()],
            2,
        ),
    ] {
        let answered = associate(&router, &service_arn, &function_id, &resources).await;
        let body = answered.ok(case);
        assert_eq!(
            body["resources"],
            json!(resources),
            "case {case:?}: the answer must report what this call associated, got {body:?}"
        );
        let counted = resource_count(&router, &service_arn, &function_id).await;
        assert_eq!(
            counted, expected_count,
            "case {case:?}: expected the function to hold {expected_count} resources, got \
             {counted}"
        );
    }
}

/// A service with NO input source has discovered nothing, so every identifier is refused —
/// including one that names a resource another stack really holds.
#[tokio::test]
async fn a_service_with_no_input_source_has_discovered_nothing() {
    let router = emulator();
    let (_, discovered) = deploy(&router, "rhv2-unsourced", &discovered_template()).await;
    let queue = discovered.get("Orders").expect("the queue's url").clone();

    let service_arn = create_service(&router, "no-input-source").await;
    let function_id = create_function(&router, &service_arn, "Checkout").await;

    for (case, identifier) in [
        ("a resource a stack holds, with no input source naming it", &queue),
        ("an identifier nothing holds", &"rhv2-ghost".to_string()),
    ] {
        let answered = associate(
            &router,
            &service_arn,
            &function_id,
            std::slice::from_ref(identifier),
        )
        .await;
        assert_eq!(
            (answered.status, answered.code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the modelled 400 ValidationException, got {} {:?} {:?}",
            answered.status,
            answered.code,
            answered.body
        );
        let expected = format!("Resource not discovered for service: {identifier}");
        let actual = answered.message();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    let counted = resource_count(&router, &service_arn, &function_id).await;
    assert_eq!(
        counted, 0,
        "a refused association must store nothing, got {counted} resources"
    );
}

/// An input source whose resources nothing in this emulator holds leaves the service's
/// resource set UNKNOWN, so nothing is refused for being outside it: the emulator never
/// refuses on the strength of a question nobody answered.
#[tokio::test]
async fn an_unreadable_input_source_leaves_nothing_refused() {
    let router = emulator();
    let service_arn = create_service(&router, "unreadable-input-source").await;

    for (case, configuration) in [
        (
            "tag-based discovery",
            json!({"resourceTags": [{"key": "Env", "values": ["prod"]}]}),
        ),
        (
            "a Terraform state file",
            json!({"tfStateFileUrl": "s3://rhv2-state/prod/terraform.tfstate"}),
        ),
    ] {
        create_input_source(&router, &service_arn, configuration).await;
        let function_id = create_function(&router, &service_arn, &format!("Fn-{case}")).await;
        let identifier = format!("arn:aws:ec2:{REGION}:000000000000:instance/i-0123456789abcdef0");
        let answered =
            associate(&router, &service_arn, &function_id, std::slice::from_ref(&identifier)).await;
        answered.ok(case);
        let counted = resource_count(&router, &service_arn, &function_id).await;
        assert_eq!(
            counted, 1,
            "case {case:?}: expected the association to be stored, got {counted} resources"
        );
    }
}

/// The resources are associated WITH a function OF a service, so a request naming either of
/// them wrongly is the modelled 404 about that resource, and nothing is stored.
#[tokio::test]
async fn a_request_naming_no_service_or_no_function_is_refused() {
    let router = emulator();
    let service_arn = create_service(&router, "missing-parents").await;
    let function_id = create_function(&router, &service_arn, "Checkout").await;
    let missing_service = format!("arn:aws:resiliencehub:{REGION}:000000000000:service/absent");

    for (case, service, function, expected) in [
        (
            "a serviceArn naming no service",
            missing_service.clone(),
            function_id.clone(),
            "Service not found: service/absent".to_string(),
        ),
        (
            "a serviceFunctionId naming no function of that service",
            service_arn.clone(),
            "absent-function".to_string(),
            "Service function not found: absent-function".to_string(),
        ),
    ] {
        let answered = associate(&router, &service, &function, &["rhv2-ghost".to_string()]).await;
        assert_eq!(
            (answered.status, answered.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected the modelled 404 ResourceNotFoundException, got {} {:?} \
             {:?}",
            answered.status,
            answered.code,
            answered.body
        );
        let actual = answered.message();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    let counted = resource_count(&router, &service_arn, &function_id).await;
    assert_eq!(
        counted, 0,
        "a refused association must store nothing, got {counted} resources"
    );
}

/// A function holds at most as many resources as the modelled `ResourceList` can carry, so an
/// association that would take it past that ceiling is refused — while one that adds nothing
/// to a function already at the ceiling is still served.
///
/// The ceiling is `ResourceList`'s own modelled `length` maximum
/// (`smithy_query(query="shape", shape="ResourceList")` reports `min 1, max 100`), which is
/// also the most identifiers one call may name, so the bound is reached in one call and
/// crossed by the next.
#[tokio::test]
async fn a_function_holds_at_most_the_resources_the_modelled_list_can_carry() {
    let router = emulator();
    let service_arn = create_service(&router, "resource-ceiling").await;
    // A source whose resources this emulator cannot read, so these rows are about the
    // ceiling rather than about discovery.
    create_input_source(
        &router,
        &service_arn,
        json!({"resourceTags": [{"key": "Env", "values": ["prod"]}]}),
    )
    .await;
    let function_id = create_function(&router, &service_arn, "Checkout").await;

    let at_the_ceiling: Vec<String> = (0..100)
        .map(|index| format!("arn:aws:s3:::rhv2-ceiling-{index}"))
        .collect();
    let filled = associate(&router, &service_arn, &function_id, &at_the_ceiling).await;
    filled.ok("the resources the list can carry");
    let counted = resource_count(&router, &service_arn, &function_id).await;
    assert_eq!(
        counted, 100,
        "case \"the resources the list can carry\": expected the function to hold 100 \
         resources, got {counted}"
    );

    // One the function already holds adds nothing, so it is still within the ceiling.
    let again = associate(
        &router,
        &service_arn,
        &function_id,
        &[at_the_ceiling[0].clone()],
    )
    .await;
    again.ok("a resource the function already holds");

    // One MORE than the list can carry.
    let past = associate(
        &router,
        &service_arn,
        &function_id,
        &["arn:aws:s3:::rhv2-ceiling-100".to_string()],
    )
    .await;
    assert_eq!(
        (past.status, past.code.as_str()),
        (400, "ValidationException"),
        "case \"one past the ceiling\": expected the modelled 400 ValidationException, got {} \
         {:?} {:?}",
        past.status,
        past.code,
        past.body
    );
    let expected =
        format!("Service function {function_id} holds at most 100 resources and already holds 100");
    let actual = past.message();
    assert_eq!(
        actual, expected,
        "case \"one past the ceiling\": expected {expected:?}, got {actual:?}"
    );

    let counted = resource_count(&router, &service_arn, &function_id).await;
    assert_eq!(
        counted, 100,
        "case \"one past the ceiling\": a refused association must store nothing, got \
         {counted} resources"
    );
}
