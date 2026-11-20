// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` whose template NO LONGER states a property of an API Gateway v2 resource
//! the stack already has, end to end: the CloudFormation calls over its awsQuery wire, the
//! resource reads over API Gateway v2's own `restJson1` wire.
//!
//! RULE: a stack update brings the live resource to the declaration it is handed, so a
//! property the new template no longer states is REMOVED from that resource — through the call
//! the owning service publishes for clearing it, which is the one thing a PATCH cannot carry —
//! rather than left at the value an earlier deployment gave it. The same rule holds one level
//! down for a map whose keys are the caller's own data: a key the new template no longer
//! states is removed, while the keys it still states stay.
//!
//! The rows differ along what the rule generalises over: WHICH resource of the api carries the
//! property (the api itself, a stage, a route), whether what is dropped is a whole property or
//! ONE entry of a data-keyed map, and whether the declaration drops it or re-states it — the
//! other side of the rule, where nothing is cleared because the patch carried the new value.
//!
//! What this suite adds over `apigatewayv2_api_property_updates.rs`, which spans the changed
//! and the create-only property: that table's revisions all STATE the property they change, so
//! none of them is a removal.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::stack_lifecycle::{
    call, create_stack, member, router, settled_stack, xml_text, ACCOUNT_ID,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The two values `ProtocolType` takes (`ProtocolType`: `HTTP | WEBSOCKET`).
const HTTP: &str = "HTTP";
const WEBSOCKET: &str = "WEBSOCKET";

/// The CORS configuration the api rows are created with: `Cors`'s own members, as
/// `AWS::ApiGatewayV2::Api.CorsConfiguration` declares them.
fn cors_configuration(origin: &str) -> Value {
    json!({
        "AllowOrigins": [origin],
        "AllowMethods": ["GET"],
        "AllowHeaders": ["content-type"],
        "MaxAge": 300,
    })
}

/// The log destination a stage's access log settings name: a CloudWatch Logs log group of this
/// emulator's own account and Region, which is what `Stage$AccessLogSettings.DestinationArn`
/// carries ("the ARN of the CloudWatch Logs log group to receive access logs").
fn log_group_arn(name: &str) -> String {
    format!("arn:aws:logs:us-east-1:{ACCOUNT_ID}:log-group:{name}")
}

/// One row of the table: a resource of an api, created with a property and re-declared without
/// it.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The api the resource belongs to — `None` for a row about the api ITSELF, whose
    /// declaration is the api's own.
    child: Option<Child>,
    /// `ProtocolType` the api is created with.
    protocol: &'static str,
    /// The properties the CREATE declares for the resource under test.
    created: Value,
    /// The properties the REVISION declares for it.
    revised: Value,
    /// The dotted member paths the resource's own read must report NOTHING at once the
    /// revision is deployed — what was dropped.
    cleared: Vec<&'static str>,
    /// The `(dotted member path, value)` pairs it must still report — what was kept.
    kept: Vec<(&'static str, Value)>,
}

/// The resource inside the api a row is about: its type and the collection its read is
/// addressed under, as that type's own operations' URIs spell it.
struct Child {
    resource_type: &'static str,
    collection: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // `DeleteCorsConfiguration` (`DELETE /v2/apis/{ApiId}/cors`) is what clears an
            // api's CORS configuration, so a template that drops `CorsConfiguration` leaves
            // the api carrying none.
            case: "the api's CORS configuration, dropped",
            stack: "apigwv2-cleared-cors",
            child: None,
            protocol: HTTP,
            created: json!({
                "Name": "apigwv2-cleared-cors-api",
                "ProtocolType": HTTP,
                "Description": "before",
                "CorsConfiguration": cors_configuration("https://allowed.example.com"),
            }),
            revised: json!({
                "Name": "apigwv2-cleared-cors-api",
                "ProtocolType": HTTP,
                "Description": "after",
            }),
            cleared: vec!["corsConfiguration"],
            kept: vec![("description", json!("after"))],
        },
        Row {
            // The other side of the rule: a revision that STATES the property changes it, and
            // nothing is cleared.
            case: "the api's CORS configuration, re-stated with another origin",
            stack: "apigwv2-cleared-cors-restated",
            child: None,
            protocol: HTTP,
            created: json!({
                "Name": "apigwv2-cleared-cors-restated-api",
                "ProtocolType": HTTP,
                "CorsConfiguration": cors_configuration("https://allowed.example.com"),
            }),
            revised: json!({
                "Name": "apigwv2-cleared-cors-restated-api",
                "ProtocolType": HTTP,
                "CorsConfiguration": cors_configuration("https://other.example.com"),
            }),
            cleared: vec![],
            kept: vec![(
                "corsConfiguration.allowOrigins",
                json!(["https://other.example.com"]),
            )],
        },
        Row {
            // `DeleteAccessLogSettings` clears the access log settings of the stage it
            // addresses, so a template that drops `AccessLogSettings` leaves the stage logging
            // nothing — while its description, which the revision re-states, is changed.
            case: "a stage's access log settings, dropped",
            stack: "apigwv2-cleared-access-logs",
            child: Some(Child {
                resource_type: "AWS::ApiGatewayV2::Stage",
                collection: "stages",
            }),
            protocol: HTTP,
            created: json!({
                "StageName": "beta",
                "Description": "before",
                "AccessLogSettings": {
                    "DestinationArn": log_group_arn("apigwv2-cleared-access-logs"),
                    "Format": "$context.requestId",
                },
            }),
            revised: json!({
                "StageName": "beta",
                "Description": "after",
            }),
            cleared: vec!["accessLogSettings"],
            kept: vec![("description", json!("after"))],
        },
        Row {
            // `DeleteRouteSettings` clears the settings of ONE route key, so a template that no
            // longer states a key of `RouteSettings` leaves the stage metering that route by
            // nothing of its own, while the key it still states keeps its settings.
            case: "one key of a stage's route settings, dropped",
            stack: "apigwv2-cleared-route-settings",
            child: Some(Child {
                resource_type: "AWS::ApiGatewayV2::Stage",
                collection: "stages",
            }),
            protocol: HTTP,
            created: json!({
                "StageName": "beta",
                "RouteSettings": {
                    "GET /one": { "ThrottlingBurstLimit": 5, "ThrottlingRateLimit": 1.0 },
                    "GET /two": { "ThrottlingBurstLimit": 7, "ThrottlingRateLimit": 2.0 },
                },
            }),
            revised: json!({
                "StageName": "beta",
                "RouteSettings": {
                    "GET /two": { "ThrottlingBurstLimit": 7, "ThrottlingRateLimit": 2.0 },
                },
            }),
            cleared: vec!["routeSettings.GET /one"],
            kept: vec![("routeSettings.GET /two.throttlingBurstLimit", json!(7))],
        },
        Row {
            // `DeleteRouteRequestParameter` clears ONE request parameter of a route — a member
            // "supported only for WebSocket APIs", hence the protocol of this row — so a
            // template that no longer states one of them leaves the route requiring it no
            // longer, while the one it still states stays.
            case: "one key of a route's request parameters, dropped",
            stack: "apigwv2-cleared-request-parameters",
            child: Some(Child {
                resource_type: "AWS::ApiGatewayV2::Route",
                collection: "routes",
            }),
            protocol: WEBSOCKET,
            created: json!({
                // The connection request's own route: the only route of a WEBSOCKET api
                // that may carry request parameters, since they are the parameters of that
                // request.
                "RouteKey": "$connect",
                "RequestParameters": {
                    "route.request.header.Auth": { "Required": true },
                    "route.request.querystring.token": { "Required": false },
                },
            }),
            revised: json!({
                "RouteKey": "$connect",
                "RequestParameters": {
                    "route.request.querystring.token": { "Required": false },
                },
            }),
            cleared: vec!["requestParameters.route.request.header.Auth"],
            kept: vec![(
                "requestParameters.route.request.querystring.token.required",
                json!(false),
            )],
        },
    ]
}

#[tokio::test]
async fn a_property_an_update_no_longer_states_is_cleared_on_the_resource() {
    for row in rows() {
        let case = row.case;
        let stack = row.stack;
        let router = router();
        let template = |properties: &Value| template_for(&row, properties);

        let (code, body) = create_stack(&router, stack, &template(&row.created), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to succeed, got {code} {body}"
        );
        let (reported, reason) = settled(&router, case, stack).await;
        assert_eq!(
            reported,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {reported:?} ({reason:?})"
        );
        let api_id = published_id(&router, case, stack, "Api").await;
        let identity = match row.child {
            Some(_) => published_id(&router, case, stack, "Child").await,
            None => api_id.clone(),
        };
        // The property really was there before the revision: a row that asserts its removal
        // asserts a CHANGE and not a value that was never written.
        let before = read_back(&router, case, &row, &api_id, &identity).await;
        for path in &row.cleared {
            assert!(
                held_member(&before, path).is_some(),
                "case {case:?}: expected the create to leave {path} on the resource, got {before}"
            );
        }

        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &template(&row.revised)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected UpdateStack to succeed, got {code} {body}"
        );
        let (reported, reason) = settled(&router, case, stack).await;
        assert_eq!(
            reported,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete, got {reported:?} ({reason:?})"
        );

        // The resource kept its identity, so the change was applied to the resource the stack
        // already had rather than to one that replaced it.
        let api_after = published_id(&router, case, stack, "Api").await;
        assert_eq!(
            api_after, api_id,
            "case {case:?}: expected the api to keep its ApiId, got {api_after:?}"
        );

        let after = read_back(&router, case, &row, &api_id, &identity).await;
        for path in &row.cleared {
            let found = held_member(&after, path);
            assert_eq!(
                found, None,
                "case {case:?}: expected API Gateway v2 to report nothing at {path} once the \
                 template no longer states it, got {found:?} ({after})"
            );
        }
        for (path, value) in &row.kept {
            let found = held_member(&after, path);
            assert_eq!(
                found.as_ref(),
                Some(value),
                "case {case:?}: expected API Gateway v2 to hold {path}={value}, got {found:?} \
                 ({after})"
            );
        }
    }
}

/// The template of one row, with `properties` declared for the resource under test: the api
/// alone for a row about the api itself, and the api plus the child that names it for a row
/// about a resource inside one.
fn template_for(row: &Row, properties: &Value) -> String {
    let stack = row.stack;
    match row.child.as_ref() {
        None => json!({
            "Resources": {
                "Api": { "Type": "AWS::ApiGatewayV2::Api", "Properties": properties }
            }
        })
        .to_string(),
        Some(child) => {
            let mut declared = properties.clone();
            declared["ApiId"] = json!({ "Ref": "Api" });
            let mut api = json!({
                "Name": format!("{stack}-api"),
                "ProtocolType": row.protocol,
            });
            if row.protocol == WEBSOCKET {
                // A WEBSOCKET api states how a frame picks its route, which `CreateApi`
                // requires of that protocol.
                api["RouteSelectionExpression"] = json!("$request.body.action");
            }
            json!({
                "Resources": {
                    "Api": { "Type": "AWS::ApiGatewayV2::Api", "Properties": api },
                    "Child": { "Type": child.resource_type, "Properties": declared },
                }
            })
            .to_string()
        }
    }
}

/// The record API Gateway v2 holds for the resource under test, read through that resource's
/// own read.
async fn read_back(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    api_id: &str,
    identity: &str,
) -> Value {
    let uri = match row.child.as_ref() {
        None => format!("/v2/apis/{api_id}"),
        Some(child) => format!("/v2/apis/{api_id}/{}/{identity}", child.collection),
    };
    let (code, held) = apigateway(router, "GET", &uri).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the resource's own read ({uri}) to answer 200, got {code} \
         with {held}"
    );
    held
}

/// The value a reported record carries at a dotted member `path`, or `None` where it carries
/// none. A segment may itself contain a `.`, as a data-keyed map's own key may
/// (`route.request.header.Auth`), so the longest key the object holds at each step wins.
fn held_member(record: &Value, path: &str) -> Option<Value> {
    if let Some(found) = record.get(path) {
        return Some(found.clone());
    }
    let members = record.as_object()?;
    // The longest member name the path begins with, so a key carrying dots is matched whole
    // before a shorter prefix of it is.
    let mut candidates: Vec<&String> = members
        .keys()
        .filter(|name| {
            path.strip_prefix(name.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
        })
        .collect();
    candidates.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for name in candidates {
        let rest = &path[name.len() + 1..];
        if let Some(found) = held_member(&members[name], rest) {
            return Some(found);
        }
    }
    None
}

/// The `StackStatus` and `StackStatusReason` a stack settled on.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let described = settled_stack(router, case, stack).await;
    (
        xml_text(&described, "StackStatus").unwrap_or_default(),
        xml_text(&described, "StackStatusReason").unwrap_or_default(),
    )
}

/// The `PhysicalResourceId` the stack publishes for one of its logical resources.
async fn published_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResource({logical_id}) to answer 200, got {code} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}
