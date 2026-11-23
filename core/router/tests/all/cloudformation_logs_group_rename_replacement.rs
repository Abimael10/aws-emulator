// SPDX-License-Identifier: CC0-1.0

//! Renaming a CloudFormation-provisioned `AWS::Logs::LogGroup`: the replacement the rename
//! requires is carried out, and the children that live INSIDE that group are re-created
//! against the group now declared.
//!
//! RULE under test: **CloudFormation refuses a planned replacement only when the new physical
//! resource would have to be created under the identity the resource it replaces still holds;
//! a resource whose name — or the parent that name identifies it within — is read off another
//! resource THIS UPDATE RECREATES has an identity of its own to be created under, so that
//! replacement is performed.** A log group's `LogGroupName` is create-only, so renaming it
//! replaces the group; a log stream and a metric filter are each identified by their own name
//! TOGETHER WITH the log group that holds them (`CreateLogStream`, `DeleteLogStream`,
//! `PutLogEvents`, `PutMetricFilter` and `DeleteMetricFilter` each take `logGroupName` beside
//! the child's name), so a child carried into the renamed group collides with nothing there
//! and is re-created under it, whether the template names the child itself or leaves the name
//! to CloudFormation.
//!
//! Two crates have to agree for that to hold — CloudFormation plans and makes the calls,
//! CloudWatch Logs decides what exists afterwards — so the suite lives here: a crate-local
//! test can only ever see one side of the call.
//!
//! The rows differ along what the rule generalises over: how the child reads the renamed group
//! (as the PARENT its name is scoped by, and as the NAME it is composed from), whether the
//! child's name is the template's or CloudFormation's to generate, how many children sit under
//! the group, which kinds of child they are (a stream, a metric filter), and which retention —
//! a property no replacement is required for — the group carries across the rename. Every
//! expected value is computed here from what the row declares.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`), and the stack-status polling every suite that deploys a stack
// shares (`tests/all/cloudformation_firehose_stream_stabilisation.rs`).
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// CloudWatch Logs' `X-Amz-Target` prefix, from its model's service shape (`Logs_20140328`),
/// and the signing name its credential scope carries (`sigv4Name: logs`).
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// The parameter the template takes the group's name from, which is the only thing the second
/// deployment changes.
const GROUP_NAME_PARAMETER: &str = "GroupName";

/// The logical ids the templates below declare.
const GROUP: &str = "Group";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from. The signature is never checked:
/// this router runs with request authentication off.
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudWatch Logs operation over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{LOGS_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(LOGS_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Every resource failure the stack recorded, so a case whose stack did not complete can say
/// WHY.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The log group CloudWatch Logs holds under `name`, as `DescribeLogGroups` reports it, or
/// `None` when it holds none.
async fn described_group(router: &MultiServiceRouter, case: &str, name: &str) -> Option<Value> {
    let (status, parsed) = logs(
        router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogGroups for {name:?}, got {status} \
         with body {parsed}"
    );
    parsed
        .get("logGroups")
        .and_then(Value::as_array)
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group.get("logGroupName").and_then(Value::as_str) == Some(name))
                .cloned()
        })
}

/// The names of the streams CloudWatch Logs holds in `group`, sorted so a row compares a SET.
async fn stream_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, parsed) = logs(
        router,
        "DescribeLogStreams",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogStreams for {group:?}, got {status} \
         with body {parsed}"
    );
    let mut names: Vec<String> = parsed
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| {
                    stream
                        .get("logStreamName")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The names of the metric filters CloudWatch Logs holds on `group`, sorted the same way.
async fn metric_filter_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, parsed) = logs(
        router,
        "DescribeMetricFilters",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeMetricFilters for {group:?}, got {status} \
         with body {parsed}"
    );
    let mut names: Vec<String> = parsed
        .get("metricFilters")
        .and_then(Value::as_array)
        .map(|filters| {
            filters
                .iter()
                .filter_map(|filter| {
                    filter
                        .get("filterName")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Wall-clock milliseconds, the unit every CloudWatch Logs timestamp member carries:
/// `PutLogEvents` admits an event against a clock-relative ingestion window, so a case that
/// writes one stamps it from the clock rather than from a fixed instant.
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}

/// One declared child of the group: its logical id, its type and the properties it states
/// beside the `LogGroupName` that names the group it lives in.
struct Child {
    logical_id: &'static str,
    resource_type: &'static str,
    properties: Value,
}

/// The template a row deploys: the group, named from the `GroupName` parameter and carrying
/// `retention`, then the children in a strict dependency chain — each child declared on the
/// one before it, which is the order a stack that declares a stream and a filter in one group
/// states.
fn template(retention: Option<i64>, children: &[Child]) -> String {
    let mut group_properties = json!({"LogGroupName": {"Ref": GROUP_NAME_PARAMETER}});
    if let Some(retention) = retention {
        group_properties["RetentionInDays"] = json!(retention);
    }
    let mut resources = json!({
        GROUP: {"Type": "AWS::Logs::LogGroup", "Properties": group_properties},
    });
    let mut previous = GROUP.to_string();
    for child in children {
        let mut properties = child.properties.clone();
        properties["LogGroupName"] = json!({"Ref": GROUP});
        resources[child.logical_id] = json!({
            "Type": child.resource_type,
            "DependsOn": previous,
            "Properties": properties,
        });
        previous = child.logical_id.to_string();
    }
    let outputs: Value = children
        .iter()
        .map(|child| {
            (
                format!("{}Ref", child.logical_id),
                json!({"Value": {"Ref": child.logical_id}}),
            )
        })
        .chain([(
            "GroupRef".to_string(),
            json!({"Value": {"Ref": GROUP}}),
        )])
        .collect::<serde_json::Map<String, Value>>()
        .into();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {GROUP_NAME_PARAMETER: {"Type": "String"}},
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// One stream declaration, with the name the row states — `None` leaving the name to
/// CloudFormation.
fn stream(logical_id: &'static str, name: Option<Value>) -> Child {
    let properties = match name {
        Some(name) => json!({"LogStreamName": name}),
        None => json!({}),
    };
    Child {
        logical_id,
        resource_type: "AWS::Logs::LogStream",
        properties,
    }
}

/// One metric filter declaration on the group, with the valid pattern and transformation
/// `PutMetricFilter` takes.
fn metric_filter(logical_id: &'static str, name: &str) -> Child {
    Child {
        logical_id,
        resource_type: "AWS::Logs::MetricFilter",
        properties: json!({
            "FilterName": name,
            "FilterPattern": "{ $.latency > 0 }",
            "MetricTransformations": [{
                "MetricName": "Latency",
                "MetricNamespace": "App",
                "MetricValue": "$.latency",
            }],
        }),
    }
}

/// What the rule says the renamed group must hold as STREAMS.
enum ExpectedStreams {
    /// The names the row states, computed from the group name now in force.
    Named(fn(&str) -> Vec<String>),
    /// One stream whose name CloudFormation GENERATES: the name the stack publishes for the
    /// replacement is what the group must hold.
    Generated,
}

/// One row: the stack it deploys, the group names the two deployments give it, and what the
/// rule says CloudWatch Logs must hold once the rename has completed.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The retention the group declares, carried across the rename — `None` for a group whose
    /// events "are always retained and never expire".
    retention: Option<i64>,
    /// The children declared under the group.
    children: Vec<Child>,
    /// The stream names the renamed group must hold ([`ExpectedStreams`]).
    expected_streams: ExpectedStreams,
    /// The metric filter names the renamed group must carry.
    expected_filters: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        // The recorded shape: a group with a NAMED stream and a NAMED metric filter under it,
        // strictly ordered. Both children read the group as the parent their own name is
        // scoped by, so both are re-created under the group now declared.
        Row {
            case: "a named stream and a named metric filter under the renamed group",
            stack: "rename-named-children",
            retention: Some(3),
            children: vec![
                stream("Stream", Some(json!("app-stream"))),
                metric_filter("Filter", "latency"),
            ],
            expected_streams: ExpectedStreams::Named(|_| vec!["app-stream".to_string()]),
            expected_filters: vec!["latency"],
        },
        // The child's own NAME composed out of the renamed group's, which is the other half of
        // the identity: the replacement is created under a name derived from the group now in
        // force.
        Row {
            case: "a stream whose own name is composed from the renamed group's name",
            stack: "rename-composed-name",
            retention: Some(7),
            children: vec![stream(
                "Stream",
                Some(json!({"Fn::Sub": format!("${{{GROUP}}}/app")})),
            )],
            expected_streams: ExpectedStreams::Named(|group| vec![format!("{group}/app")]),
            expected_filters: Vec::new(),
        },
        // A child the template does not name at all: CloudFormation generates the
        // replacement's name, and the group now declared holds exactly that stream.
        Row {
            case: "a stream whose name CloudFormation generates",
            stack: "rename-generated-name",
            retention: None,
            children: vec![stream("Stream", None)],
            expected_streams: ExpectedStreams::Generated,
            expected_filters: Vec::new(),
        },
        // SEVERAL children under one replaced group, so the rule is asserted over every child
        // the chain declares rather than over the first one the walk reaches.
        Row {
            case: "two named streams and two named metric filters under the renamed group",
            stack: "rename-many-children",
            retention: Some(1),
            children: vec![
                stream("First", Some(json!("first-stream"))),
                stream("Second", Some(json!("second-stream"))),
                metric_filter("Latency", "latency"),
                metric_filter("Errors", "errors"),
            ],
            expected_streams: ExpectedStreams::Named(|_| {
                vec!["first-stream".to_string(), "second-stream".to_string()]
            }),
            expected_filters: vec!["errors", "latency"],
        },
    ]
}

/// A `DescribeStacks` answer's output under `key`.
fn output(xml: &str, key: &str) -> Option<String> {
    crate::query_wire::outputs_of(xml).get(key).cloned()
}

#[tokio::test]
async fn renaming_a_declared_log_group_replaces_it_and_recreates_the_children_under_it() {
    let router = build_router();
    for row in rows() {
        let case = row.case;
        let body = template(row.retention, &row.children);
        let before = format!("/{}/before", row.stack);
        let after = format!("/{}/after", row.stack);

        let (status, answer) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Parameters.member.1.ParameterKey", GROUP_NAME_PARAMETER),
                ("Parameters.member.1.ParameterValue", &before),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answer}"
        );
        let (reported, created) = wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {created} with the resource \
             failures {:?}",
            failure_reasons(&router, row.stack).await
        );

        // One event in the stream the create made, so the row can show the replacement is a
        // NEW stream rather than the one that was there.
        let stream_in_force = output(&created, "StreamRef")
            .or_else(|| output(&created, "FirstRef"))
            .unwrap_or_default();
        assert!(
            !stream_in_force.is_empty(),
            "case {case:?}: expected the stack to publish a stream name, got {created}"
        );
        let (status, document) = logs(
            &router,
            "PutLogEvents",
            &json!({
                "logGroupName": before,
                "logStreamName": stream_in_force,
                "logEvents": [{"timestamp": now_ms(), "message": "before the rename"}],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the group the create made to accept an event, got {status} \
             with {document}"
        );

        // The rename: the same template, with only the group's name parameter changed.
        let (status, answer) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", row.stack),
                ("UsePreviousTemplate", "true"),
                ("Parameters.member.1.ParameterKey", GROUP_NAME_PARAMETER),
                ("Parameters.member.1.ParameterValue", &after),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answer}"
        );
        let (reported, settled) =
            wait_for_stack_status(&router, row.stack, UPDATE_COMPLETE).await;
        assert_eq!(
            reported, UPDATE_COMPLETE,
            "case {case:?}: expected the rename to complete, got {settled} with the resource \
             failures {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The stack reports the replacement group as the resource it now holds...
        let physical_ids = physical_ids_of(
            &cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)])
                .await
                .1,
        );
        assert_eq!(
            physical_ids.get(GROUP).map(String::as_str),
            Some(after.as_str()),
            "case {case:?}: expected the group's physical id to be the name now declared, got \
             {physical_ids:?}"
        );
        assert_eq!(
            output(&settled, "GroupRef").as_deref(),
            Some(after.as_str()),
            "case {case:?}: expected the stack to publish the replacement group's name, got \
             {settled}"
        );

        // ...and CloudWatch Logs holds it, with the retention the declaration asks for and
        // nothing of the group it replaced.
        let group = described_group(&router, case, &after)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the renamed group to exist"));
        assert_eq!(
            group.get("retentionInDays").and_then(Value::as_i64),
            row.retention,
            "case {case:?}: expected the declared retention to be carried to the replacement, \
             got {group}"
        );
        assert_eq!(
            described_group(&router, case, &before).await,
            None,
            "case {case:?}: expected the replaced group to have been deleted"
        );

        // Every child the template declares was re-created under the group now declared.
        let expected_streams: Vec<String> = match row.expected_streams {
            ExpectedStreams::Named(names) => names(&after),
            // The name the stack publishes for the replacement is what the group must hold.
            ExpectedStreams::Generated => output(&settled, "StreamRef").into_iter().collect(),
        };
        let held = stream_names(&router, case, &after).await;
        assert_eq!(
            held, expected_streams,
            "case {case:?}: expected the renamed group to hold the re-created streams, got \
             {held:?}"
        );
        let filters = metric_filter_names(&router, case, &after).await;
        assert_eq!(
            filters,
            row.expected_filters
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {case:?}: expected the renamed group to carry the re-created metric filters, \
             got {filters:?}"
        );

        // The children really exist in CloudWatch Logs: the group takes an event on the
        // re-created stream, and carries none of the ones written before the rename.
        let stream = expected_streams
            .first()
            .cloned()
            .expect("every row declares at least one stream");
        let (status, document) = logs(
            &router,
            "GetLogEvents",
            &json!({"logGroupName": after, "logStreamName": stream}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the re-created stream to be readable, got {status} with \
             {document}"
        );
        assert_eq!(
            document
                .get("events")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0),
            "case {case:?}: expected the re-created stream to carry none of the replaced \
             group's events, got {document}"
        );
        let (status, document) = logs(
            &router,
            "PutLogEvents",
            &json!({
                "logGroupName": after,
                "logStreamName": stream,
                "logEvents": [{"timestamp": now_ms(), "message": "after the rename"}],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the renamed group to accept an event on the re-created \
             stream, got {status} with {document}"
        );
    }
}
