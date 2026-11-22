// SPDX-License-Identifier: CC0-1.0

//! A Resource Groups group whose definition is a `CLOUDFORMATION_STACK_1_0`
//! resource query really holds the resources the CloudFormation STACK it names
//! holds, asserted over the production service set
//! (`aws_emulator_router::build_router` on one engine, exactly as the binary
//! assembles it).
//!
//! The RULE pinned here: a `CLOUDFORMATION_STACK_1_0` query resolves to the
//! resources of the CloudFormation stack its `StackIdentifier` names — each
//! reported under the ARN the service that OWNS it minted and the type
//! CloudFormation records for it, limited to the types the query's
//! `ResourceTypeFilters` admits — and the modelled
//! `CLOUDFORMATION_STACK_NOT_EXISTING` query error is reported only for an
//! identifier the account holds no stack under. `ListGroupResources` and
//! `SearchResources` both name the stack reads among their required permissions
//! ("cloudformation:DescribeStacks cloudformation:ListStackResources"), which is
//! that view.
//!
//! Three crates have to agree for this to hold — the services that own the stack's
//! resources, CloudFormation, which installs them, and Resource Groups, which
//! resolves membership from the stack — so the suite lives here rather than in any
//! of them.
//!
//! The rows differ along what the rule generalises over: which types the query
//! admits and how many, `AWS::AllSupported` and no type element at all, a supported
//! type the stack holds no resource of, whether the stack is named by its NAME or by
//! its id, and an identifier of neither form that resolves to nothing. The same
//! table is asserted through `ListGroupResources` (the stored query of a group) and
//! through `SearchResources` (the query a request carries), since one resolution
//! serves both. Nothing waits on the wall clock: a stack is polled until it settles.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request below is signed for, and so the Region the stack, its
/// resources and the groups live in.
const REGION: &str = "us-west-2";

/// The URIs the model binds the operations this suite drives to
/// (`smithy_query(query="operation",operation="CreateGroup,ListGroupResources,SearchResources")`).
const CREATE_GROUP: &str = "/groups";
const LIST: &str = "/list-group-resources";
const SEARCH: &str = "/resources/search";

/// The CloudFormation API version an awsQuery request names beside its action, from
/// that service's own model.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// How long a wait for a stack to reach a TERMINAL status may run before the suite
/// calls it a failure. A stack's own clock-owed spans are far shorter; this is a
/// safety net, generous enough that a loaded host cannot decide the outcome, and
/// nothing here is asserted about elapsed time.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait between two looks at a condition that is not satisfied yet.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The CloudFormation-style types the stack's resources are of, plus a supported
/// type the stack holds no resource of, from the AWS CloudFormation resource and
/// property types reference — the vocabulary `ResourceIdentifier$ResourceType` is
/// documented in (`smithy_query(query="shape",shape="ResourceIdentifier")`).
const TOPIC_TYPE: &str = "AWS::SNS::Topic";
const QUEUE_TYPE: &str = "AWS::SQS::Queue";
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";

/// The `ResourceTypeFilters` value that names no limit at all: it makes a query
/// "match resources of any resource type that also match the query"
/// (`smithy_query(query="doc",shape="ResourceQuery",member="Query")`).
const ALL_SUPPORTED: &str = "AWS::AllSupported";

/// The modelled `QueryErrorCode` for a stack the account holds none of
/// (`smithy_query(query="shape",shape="QueryErrorCode")`).
const STACK_NOT_EXISTING: &str = "CLOUDFORMATION_STACK_NOT_EXISTING";

/// The whole emulator over one engine, exactly as the binary assembles it, so which
/// service claims each call is decided by the production mount.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern: the `Authorization`
    // header below is what routes a request and tells a handler its Region.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`].
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One CloudFormation call over its awsQuery wire, as its status and the XML text it
/// answered.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={CLOUDFORMATION_API_VERSION}");
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
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must answer the modelled 200.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case CloudFormation {action}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// One Resource Groups call over its restJson1 wire, as its status and parsed body.
async fn resource_groups(router: &MultiServiceRouter, path: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", format!("resource-groups.{REGION}.amazonaws.com"))
        .header("authorization", authorization("resource-groups"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Resource Groups call that must answer the modelled 200.
async fn resource_groups_ok(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = resource_groups(router, path, body).await;
    assert_eq!(
        status, 200,
        "case {case}: expected HTTP 200 from POST {path}, got {status} with {parsed}"
    );
    parsed
}

/// The stack's status once it is no longer `*_IN_PROGRESS`, beside the answer it was
/// read from: looked at again until it settles or [`TERMINAL_DEADLINE`] passes, so
/// what the suite waits for is the CONDITION rather than a span of the host's clock.
async fn settled_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = cloudformation_ok(router, "DescribeStacks", &[("StackName", stack)]).await;
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return (reached, described);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The template every stack below deploys: one Amazon SNS topic and one Amazon SQS
/// queue, two resources of two different owning services, with each resource's ARN
/// published as a stack output — the value that service minted, which is what the
/// expectations below are computed from.
fn template(suffix: &str) -> String {
    json!({
        "Resources": {
            "Topic": {
                "Type": TOPIC_TYPE,
                "Properties": { "TopicName": format!("rg-stack-topic-{suffix}") }
            },
            "Queue": {
                "Type": QUEUE_TYPE,
                "Properties": { "QueueName": format!("rg-stack-queue-{suffix}") }
            }
        },
        "Outputs": {
            // "Ref returns the topic ARN" for an `AWS::SNS::Topic`, and
            // `Fn::GetAtt "Arn"` is the queue's ARN for an `AWS::SQS::Queue`.
            "TopicArn": { "Value": { "Ref": "Topic" } },
            "QueueArn": { "Value": { "Fn::GetAtt": ["Queue", "Arn"] } }
        }
    })
    .to_string()
}

/// A deployed stack: how it is addressed, and the ARNs its own resources were minted
/// under.
struct Stack {
    /// The name the stack was created under.
    name: String,
    /// The stack's own id, which a `StackIdentifier` may name it by instead.
    id: String,
    /// The ARN Amazon SNS minted for the stack's topic.
    topic_arn: String,
    /// The ARN Amazon SQS minted for the stack's queue.
    queue_arn: String,
}

/// Deploy [`template`] as the stack `name` and read back what the services that own
/// its resources minted, waiting for the stack to settle at `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, name: &str) -> Stack {
    let body = template(name);
    cloudformation_ok(
        router,
        "CreateStack",
        &[("StackName", name), ("TemplateBody", &body)],
    )
    .await;
    let (reached, described) = settled_stack(router, name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {name}: expected the stack to reach CREATE_COMPLETE, got {reached:?} in {described}"
    );
    let outputs = outputs_of(&described);
    let published = |key: &str| {
        outputs
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("case {name}: expected the output {key} in {described}"))
    };
    Stack {
        name: name.to_string(),
        id: opt_element(&described, "StackId")
            .unwrap_or_else(|| panic!("case {name}: expected a StackId in {described}")),
        topic_arn: published("TopicArn"),
        queue_arn: published("QueueArn"),
    }
}

/// Create the group `name` defined by the stack query `document`. The model carries
/// a `ResourceQuery`'s `Query` as a JSON STRING inside the structure
/// (`smithy_query(query="shape",shape="ResourceQuery")`).
async fn stack_query_group(router: &MultiServiceRouter, name: &str, document: &Value) {
    resource_groups_ok(
        router,
        name,
        CREATE_GROUP,
        &json!({
            "Name": name,
            "ResourceQuery": {
                "Type": "CLOUDFORMATION_STACK_1_0",
                "Query": document.to_string()
            }
        }),
    )
    .await;
}

/// The `(ARN, ResourceType)` pairs a `ResourceIdentifiers` list reports, in the order
/// it reports them.
fn identifiers(case: &str, page: &Value) -> Vec<(String, Option<String>)> {
    page["ResourceIdentifiers"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected ResourceIdentifiers, got {page}"))
        .iter()
        .map(|identifier| {
            (
                identifier["ResourceArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: expected a ResourceArn, got {page}"))
                    .to_string(),
                identifier["ResourceType"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// The `QueryErrors` a page reports, as `(ErrorCode, Message)` pairs.
fn query_errors(case: &str, page: &Value) -> Vec<(String, String)> {
    page["QueryErrors"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected QueryErrors, got {page}"))
        .iter()
        .map(|error| {
            (
                error["ErrorCode"].as_str().unwrap_or_default().to_string(),
                error["Message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// One row of the query table: the `Query` document, and the membership plus the
/// query errors the rule says resolving it must answer.
struct Row {
    case: &'static str,
    document: Value,
    /// The `(ARN, ResourceType)` pairs the query selects, in ARN order.
    expected: Vec<(String, Option<String>)>,
    /// The `(ErrorCode, Message)` pairs resolving the query must report.
    errors: Vec<(String, String)>,
}

/// The member pair a resource of `resource_type` carrying `arn` is reported as.
fn member(arn: &str, resource_type: &str) -> (String, Option<String>) {
    (arn.to_string(), Some(resource_type.to_string()))
}

/// Both of the stack's resources, in the ARN order a membership is reported in.
fn both(stack: &Stack) -> Vec<(String, Option<String>)> {
    let mut members = vec![
        member(&stack.topic_arn, TOPIC_TYPE),
        member(&stack.queue_arn, QUEUE_TYPE),
    ];
    members.sort();
    members
}

/// The table every case below is asserted over, computed from what the owning
/// services minted for `stack`'s resources.
fn rows(stack: &Stack) -> Vec<Row> {
    let absent = "rg-stack-nothing-holds";
    vec![
        Row {
            case: "every supported type",
            document: json!({
                "ResourceTypeFilters": [ALL_SUPPORTED],
                "StackIdentifier": stack.name.clone()
            }),
            expected: both(stack),
            errors: Vec::new(),
        },
        Row {
            case: "no type element at all",
            document: json!({ "StackIdentifier": stack.name.clone() }),
            expected: both(stack),
            errors: Vec::new(),
        },
        Row {
            case: "one type: the topic",
            document: json!({
                "ResourceTypeFilters": [TOPIC_TYPE],
                "StackIdentifier": stack.name.clone()
            }),
            expected: vec![member(&stack.topic_arn, TOPIC_TYPE)],
            errors: Vec::new(),
        },
        Row {
            case: "one type: the queue",
            document: json!({
                "ResourceTypeFilters": [QUEUE_TYPE],
                "StackIdentifier": stack.name.clone()
            }),
            expected: vec![member(&stack.queue_arn, QUEUE_TYPE)],
            errors: Vec::new(),
        },
        Row {
            case: "two types named together",
            document: json!({
                "ResourceTypeFilters": [QUEUE_TYPE, TOPIC_TYPE],
                "StackIdentifier": stack.name.clone()
            }),
            expected: both(stack),
            errors: Vec::new(),
        },
        Row {
            case: "a supported type the stack holds none of",
            document: json!({
                "ResourceTypeFilters": [TABLE_TYPE],
                "StackIdentifier": stack.name.clone()
            }),
            expected: Vec::new(),
            errors: Vec::new(),
        },
        Row {
            case: "the stack named by its id",
            document: json!({
                "ResourceTypeFilters": [ALL_SUPPORTED],
                "StackIdentifier": stack.id.clone()
            }),
            expected: both(stack),
            errors: Vec::new(),
        },
        Row {
            case: "an identifier the account holds no stack under",
            document: json!({
                "ResourceTypeFilters": [ALL_SUPPORTED],
                "StackIdentifier": absent
            }),
            expected: Vec::new(),
            errors: vec![(
                STACK_NOT_EXISTING.to_string(),
                format!("Stack with id {absent} does not exist"),
            )],
        },
    ]
}

/// A group defined by a stack query lists the resources of the stack it names, each
/// under the ARN its owning service minted and the type CloudFormation records for
/// it — and the same query carried by a `SearchResources` request selects the same
/// resources, since one resolution serves both.
#[tokio::test]
async fn a_stack_query_resolves_the_stacks_own_resources() {
    let router = router();
    let stack = deploy(&router, "rg-stack-membership").await;

    for (index, row) in rows(&stack).into_iter().enumerate() {
        let group = format!("stack-derived-{index}");
        stack_query_group(&router, &group, &row.document).await;

        let listed = resource_groups_ok(&router, row.case, LIST, &json!({ "Group": &group })).await;
        let reported = identifiers(row.case, &listed);
        assert_eq!(
            reported, row.expected,
            "case {}: expected the stack's membership {:?}, got {reported:?}",
            row.case, row.expected
        );
        let reported = query_errors(row.case, &listed);
        assert_eq!(
            reported, row.errors,
            "case {}: expected the query errors {:?}, got {reported:?}",
            row.case, row.errors
        );
        // `Resources` and the deprecated `ResourceIdentifiers` report one
        // membership, so a client reading either reads the same resources.
        let items: Vec<Value> = listed["Resources"]
            .as_array()
            .unwrap_or_else(|| panic!("case {}: expected Resources, got {listed}", row.case))
            .iter()
            .map(|item| item["Identifier"].clone())
            .collect();
        assert_eq!(
            listed["ResourceIdentifiers"].as_array(),
            Some(&items),
            "case {}: expected ResourceIdentifiers to mirror Resources, got {listed}",
            row.case
        );

        let searched = resource_groups_ok(
            &router,
            row.case,
            SEARCH,
            &json!({
                "ResourceQuery": {
                    "Type": "CLOUDFORMATION_STACK_1_0",
                    "Query": row.document.to_string()
                }
            }),
        )
        .await;
        let reported = identifiers(row.case, &searched);
        assert_eq!(
            reported, row.expected,
            "case {} through SearchResources: expected {:?}, got {reported:?}",
            row.case, row.expected
        );
        let reported = query_errors(row.case, &searched);
        assert_eq!(
            reported, row.errors,
            "case {} through SearchResources: expected the query errors {:?}, got \
             {reported:?}",
            row.case, row.errors
        );
    }
}

/// The membership is DERIVED from the stack every time it is read, so it follows
/// what CloudFormation holds: a resource the stack installed is listed under the
/// physical id CloudFormation records for it, and once the stack is deleted the
/// group that names it by NAME resolves no stack at all.
#[tokio::test]
async fn a_stack_query_membership_follows_the_stack() {
    let router = router();
    let stack = deploy(&router, "rg-stack-following").await;
    let group = "stack-following";
    stack_query_group(
        &router,
        group,
        &json!({
            "ResourceTypeFilters": [ALL_SUPPORTED],
            "StackIdentifier": stack.name.clone()
        }),
    )
    .await;

    // What the stack itself reports is what the membership is reported under: the
    // ARNs the owning services minted, which for an `AWS::SNS::Topic` is the very
    // physical id CloudFormation records ("Ref returns the topic ARN").
    let resources =
        cloudformation_ok(&router, "DescribeStackResources", &[("StackName", &stack.name)]).await;
    let recorded: BTreeMap<String, String> = physical_ids_of(&resources);
    let listed = resource_groups_ok(&router, "deployed", LIST, &json!({ "Group": group })).await;
    let members: Vec<String> = identifiers("deployed", &listed)
        .into_iter()
        .map(|(arn, _)| arn)
        .collect();
    let expected: Vec<String> = both(&stack).into_iter().map(|(arn, _)| arn).collect();
    assert_eq!(
        members, expected,
        "case deployed: expected the membership {expected:?}, got {members:?}"
    );
    assert_eq!(
        recorded.get("Topic"),
        Some(&stack.topic_arn),
        "case deployed: expected the stack's own physical id for the topic to be the ARN \
         {:?} the membership reports, got {recorded:?}",
        stack.topic_arn
    );

    cloudformation_ok(&router, "DeleteStack", &[("StackName", &stack.name)]).await;
    // A deleted stack is addressable by its id alone, so that is what the wait for
    // its deletion reads.
    let (reached, described) = settled_stack(&router, &stack.id).await;
    assert_eq!(
        reached, "DELETE_COMPLETE",
        "case deleted: expected the stack to reach DELETE_COMPLETE, got {reached:?} in \
         {described}"
    );

    let listed = resource_groups_ok(&router, "deleted", LIST, &json!({ "Group": group })).await;
    let members = identifiers("deleted", &listed);
    assert!(
        members.is_empty(),
        "case deleted: expected no member once the stack is gone, got {members:?}"
    );
    let errors = query_errors("deleted", &listed);
    assert_eq!(
        errors,
        vec![(
            STACK_NOT_EXISTING.to_string(),
            format!("Stack with id {} does not exist", stack.name)
        )],
        "case deleted: expected the stack to resolve to nothing, got {errors:?}"
    );
}
