// SPDX-License-Identifier: CC0-1.0

//! A resource whose declared NAME is composed out of an identifier another resource's owning
//! service MINTED, and the children the same template declares inside it.
//!
//! RULE under test: **a resource is created in its owning service — and reported by the stack
//! — under the name its declaration resolves to once the resources it depends on carry the
//! identifiers their own services assigned**. So a log group whose `LogGroupName` is built
//! around an `Fn::GetAtt`/`Ref` of a service-assigned identifier exists under exactly the name
//! a reader of that same identifier builds, and a `Ref` to the group from a metric filter, a
//! subscription filter or a log stream in the same template reaches it.
//!
//! The reported case paired an `AWS::Logs::LogGroup` named `/aws/appsync/apis/<apiId>` (the api
//! id read off an `AWS::AppSync::GraphQLApi` in the same stack) with an
//! `AWS::Logs::MetricFilter` over it: the group was created under the STAND-IN id
//! CloudFormation had computed before AppSync minted the real one, so every declaration
//! resolved after that create — a resource withheld behind a dependency still coming up, an
//! output, a client's own call — named a group CloudWatch Logs held nothing under, and the
//! child's create was refused `ResourceNotFoundException`, which rolled the whole stack back.
//!
//! The rows differ along what the rule generalises over rather than restating that template:
//!
//! * WHICH service mints the identifier the name is composed from, and HOW the declaration
//!   reads it — AWS AppSync's api id through an `Fn::GetAtt` of a published attribute, and
//!   Amazon API Gateway v2's api id through a `Ref` to the resource itself — against a control
//!   row whose name is a literal, which no minting touches;
//! * HOW the template composes it — `Fn::Join` with the identifier at the end of the name, and
//!   `Fn::Sub` with it in the middle of a longer one;
//! * WHICH child of the group the template declares — a metric filter, a subscription filter
//!   and a log stream — and a row that declares all three;
//! * WHETHER another resource of the same stack is still coming up while the children are
//!   created (a schema, whose creation AppSync reports asynchronously), which is what makes
//!   the deployment resolve its declarations again after the identifier has been minted.
//!
//! Every case drives the real request path for all four services through one router, so what is
//! asserted is what a client that deploys the template and then reads the resources back
//! observes. Nothing is waited out: the stack is polled to its terminal status.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_event_and_merged_api_resources::appsync;
use crate::apigateway_vpc_link_resources::apigateway;
use crate::dynamodb_table_resources::failure_reasons;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::logs_and_iot_resources::described_group;
use crate::logs_group_children_resources::{logs, stream_names};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, output_value, router, xml_text,
};

/// The namespace the metric a declared filter publishes lives in, and that metric's own name:
/// one pair for every row, so what differs between rows is the row's own shape alone.
const METRIC_NAMESPACE: &str = "Sim/GraphQL";
const METRIC_NAME: &str = "GraphQLRequests";

/// Which service mints the identifier a row's log-group name is composed out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Minter {
    /// AWS AppSync, which assigns a GraphQL API's `ApiId` (`CreateGraphqlApi` takes no
    /// identifier and answers the one it minted), read through `Fn::GetAtt`.
    AppSyncApiId,
    /// Amazon API Gateway v2, which assigns an api's id the same way — "Ref returns the API
    /// ID" — so the declaration reads it through a `Ref` to the resource itself.
    ApiGatewayV2ApiId,
    /// Nothing: the row's name is a literal the declaration states outright.
    None,
}

/// How the template composes the name out of that identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Composition {
    /// `Fn::Join`, with the identifier as the last segment of the name.
    JoinSuffix,
    /// `Fn::Sub`, with the identifier in the middle of a longer name.
    SubInfix,
    /// The name is written out.
    Literal,
}

/// One row: the estate a template declares, and the children it puts inside the group.
struct Row {
    case: &'static str,
    stack: &'static str,
    minter: Minter,
    composition: Composition,
    /// Whether the template declares an `AWS::Logs::MetricFilter` over the group.
    metric_filter: bool,
    /// Whether it declares an `AWS::Logs::SubscriptionFilter` over the group, which brings the
    /// Kinesis stream it delivers to and the role it delivers under with it.
    subscription_filter: bool,
    /// Whether it declares an `AWS::Logs::LogStream` inside the group.
    log_stream: bool,
    /// Whether the template also declares an `AWS::AppSync::GraphQLSchema`, whose creation that
    /// service reports asynchronously — so the deployment spans several passes and resolves its
    /// declarations again once the api id has been minted.
    schema: bool,
    /// The retention the declaration states, a value `AWS::Logs::LogGroup`'s `RetentionInDays`
    /// publishes.
    retention: i64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a group named after the api id AppSync minted, with a metric filter over it",
            stack: "graphql-metric-filter",
            minter: Minter::AppSyncApiId,
            composition: Composition::JoinSuffix,
            metric_filter: true,
            subscription_filter: false,
            log_stream: false,
            schema: false,
            retention: 1,
        },
        Row {
            case: "a group named after that api id inside a longer name, with every child the \
                   type publishes and a schema the deployment waits for",
            stack: "graphql-every-child",
            minter: Minter::AppSyncApiId,
            composition: Composition::SubInfix,
            metric_filter: true,
            subscription_filter: true,
            log_stream: true,
            schema: true,
            retention: 7,
        },
        Row {
            case: "a group named after that api id beside a schema the deployment waits for, \
                   with a metric filter over it",
            stack: "graphql-deferred-metric-filter",
            minter: Minter::AppSyncApiId,
            composition: Composition::JoinSuffix,
            metric_filter: true,
            subscription_filter: false,
            log_stream: false,
            schema: true,
            retention: 1,
        },
        Row {
            case: "a group named after the api id API Gateway v2 minted, read with a Ref, with \
                   a log stream in it",
            stack: "httpapi-log-stream",
            minter: Minter::ApiGatewayV2ApiId,
            composition: Composition::JoinSuffix,
            metric_filter: false,
            subscription_filter: false,
            log_stream: true,
            schema: false,
            retention: 30,
        },
        Row {
            case: "a group named after that api id inside a longer name, with a subscription \
                   filter on it",
            stack: "httpapi-subscription-filter",
            minter: Minter::ApiGatewayV2ApiId,
            composition: Composition::SubInfix,
            metric_filter: false,
            subscription_filter: true,
            log_stream: false,
            schema: false,
            retention: 90,
        },
        Row {
            case: "the control: a literal group name no minting touches, with a metric filter \
                   and a log stream",
            stack: "literal-group-children",
            minter: Minter::None,
            composition: Composition::Literal,
            metric_filter: true,
            subscription_filter: false,
            log_stream: true,
            schema: false,
            retention: 1,
        },
    ]
}

/// The logical ids the templates declare.
const API: &str = "Api";
const HTTP_API: &str = "HttpApi";
const GROUP: &str = "Group";
const DESTINATION: &str = "Destination";
const DELIVERY_ROLE: &str = "DeliveryRole";

/// The output every row publishes the group's name under, and the one the composed rows publish
/// the identifier the name is built around under.
const LOG_GROUP_NAME_OUTPUT: &str = "LogGroupName";
const IDENTIFIER_OUTPUT: &str = "Identifier";

/// The prefix a row's log-group name begins with, which is also where the name of the service
/// that minted the identifier sits.
fn prefix(minter: Minter) -> &'static str {
    match minter {
        Minter::AppSyncApiId => "/aws/appsync/apis/",
        Minter::ApiGatewayV2ApiId => "/aws/apigateway/http-apis/",
        Minter::None => "/sim/logs/",
    }
}

/// The name a row's group must end up under, built from the identifier the stack publishes for
/// the resource the name references — the very name a client reading that identifier builds.
fn expected_name(row: &Row, identifier: &str) -> String {
    match row.composition {
        Composition::JoinSuffix => format!("{}{identifier}", prefix(row.minter)),
        Composition::SubInfix => format!("{}{identifier}/requests", prefix(row.minter)),
        Composition::Literal => format!("{}{}", prefix(row.minter), row.stack),
    }
}

/// The declaration of the resource whose identifier a row's name is composed from, the document
/// its `LogGroupName` states, and the document the row publishes that identifier under.
fn named_dependency(row: &Row) -> (Option<(&'static str, Value)>, Value, Option<Value>) {
    match row.minter {
        Minter::AppSyncApiId => {
            let dependency = (
                API,
                json!({
                    "Type": "AWS::AppSync::GraphQLApi",
                    "Properties": {
                        "Name": format!("{}-api", row.stack),
                        "AuthenticationType": "API_KEY",
                    },
                }),
            );
            let identifier = json!({"Fn::GetAtt": [API, "ApiId"]});
            (
                Some(dependency),
                composed(row, identifier.clone()),
                Some(identifier),
            )
        }
        Minter::ApiGatewayV2ApiId => {
            let dependency = (
                HTTP_API,
                json!({
                    "Type": "AWS::ApiGatewayV2::Api",
                    "Properties": {
                        "Name": format!("{}-api", row.stack),
                        "ProtocolType": "HTTP",
                    },
                }),
            );
            let identifier = json!({"Ref": HTTP_API});
            (
                Some(dependency),
                composed(row, identifier.clone()),
                Some(identifier),
            )
        }
        Minter::None => (None, composed(row, Value::Null), None),
    }
}

/// The `LogGroupName` document a row declares: the prefix with the identifier composed into it,
/// or — for the control row — the name written out.
fn composed(row: &Row, identifier: Value) -> Value {
    let prefix = prefix(row.minter);
    match row.composition {
        Composition::JoinSuffix => json!({"Fn::Join": ["", [prefix, identifier]]}),
        Composition::SubInfix => json!({
            "Fn::Sub": [format!("{prefix}${{Identifier}}/requests"), {"Identifier": identifier}]
        }),
        Composition::Literal => json!(format!("{prefix}{}", row.stack)),
    }
}

/// The template one row deploys.
fn template_of(row: &Row) -> String {
    let (dependency, log_group_name, identifier) = named_dependency(row);
    let mut resources = serde_json::Map::new();
    if let Some((logical_id, declaration)) = dependency {
        resources.insert(logical_id.to_string(), declaration);
    }
    resources.insert(
        GROUP.to_string(),
        json!({
            "Type": "AWS::Logs::LogGroup",
            "Properties": {
                "LogGroupName": log_group_name,
                "RetentionInDays": row.retention,
            },
        }),
    );
    if row.metric_filter {
        resources.insert(
            "Filter".to_string(),
            json!({
                "Type": "AWS::Logs::MetricFilter",
                "Properties": {
                    "FilterName": format!("{}-requests", row.stack),
                    "LogGroupName": {"Ref": GROUP},
                    "FilterPattern": "\"GraphQL\"",
                    "MetricTransformations": [{
                        "MetricName": METRIC_NAME,
                        "MetricNamespace": METRIC_NAMESPACE,
                        "MetricValue": "1",
                    }],
                },
            }),
        );
    }
    if row.subscription_filter {
        resources.insert(
            DESTINATION.to_string(),
            json!({
                "Type": "AWS::Kinesis::Stream",
                "Properties": {"Name": format!("{}-dest", row.stack), "ShardCount": 1},
            }),
        );
        resources.insert(
            DELIVERY_ROLE.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-role", row.stack),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "logs.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            }),
        );
        resources.insert(
            "Deliver".to_string(),
            json!({
                "Type": "AWS::Logs::SubscriptionFilter",
                "Properties": {
                    "FilterName": format!("{}-deliver", row.stack),
                    "LogGroupName": {"Ref": GROUP},
                    "FilterPattern": "",
                    "DestinationArn": {"Fn::GetAtt": [DESTINATION, "Arn"]},
                    "RoleArn": {"Fn::GetAtt": [DELIVERY_ROLE, "Arn"]},
                },
            }),
        );
    }
    if row.schema {
        resources.insert(
            "Schema".to_string(),
            json!({
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": {"Fn::GetAtt": [API, "ApiId"]},
                    "Definition": "type Query { ping: String } schema { query: Query }",
                },
            }),
        );
    }
    if row.log_stream {
        resources.insert(
            "Stream".to_string(),
            json!({
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    "LogGroupName": {"Ref": GROUP},
                    "LogStreamName": format!("{}-stream", row.stack),
                },
            }),
        );
    }
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        LOG_GROUP_NAME_OUTPUT.to_string(),
        json!({"Value": {"Ref": GROUP}}),
    );
    if let Some(identifier) = identifier {
        outputs.insert(IDENTIFIER_OUTPUT.to_string(), json!({"Value": identifier}));
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// The names of the metric filters CloudWatch Logs holds on `group`.
async fn metric_filter_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    filter_names(router, "DescribeMetricFilters", "metricFilters", group).await
}

/// The names of the subscription filters CloudWatch Logs holds on `group`.
async fn subscription_filter_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    filter_names(
        router,
        "DescribeSubscriptionFilters",
        "subscriptionFilters",
        group,
    )
    .await
}

/// The `filterName`s one of the two describes reports for the filters held on `group`; the empty
/// list where that service answers no group at all, which is what a group nothing holds looks
/// like to a client.
async fn filter_names(
    router: &MultiServiceRouter,
    operation: &str,
    member: &str,
    group: &str,
) -> Vec<String> {
    let (status, document) = logs(router, operation, &json!({"logGroupName": group})).await;
    if status != 200 {
        return Vec::new();
    }
    document
        .get(member)
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
        .unwrap_or_default()
}

/// Whether the service that MINTED `identifier` really holds the resource under it — the read a
/// client makes with the identifier the stack published.
///
/// This is what tells the identifier a service assigned from the stand-in CloudFormation
/// computed before the create: a stand-in resolves to nothing in the owning service, so a name
/// built around one names a resource nobody can reach.
async fn owning_service_holds(
    router: &MultiServiceRouter,
    minter: Minter,
    identifier: &str,
) -> (u16, Value) {
    match minter {
        // `GetGraphqlApi` is `GET /v1/apis/{apiId}`.
        Minter::AppSyncApiId => appsync(router, &format!("/v1/apis/{identifier}")).await,
        // `GetApi` is `GET /v2/apis/{ApiId}`.
        Minter::ApiGatewayV2ApiId => {
            apigateway(router, "GET", &format!("/v2/apis/{identifier}")).await
        }
        Minter::None => (200, Value::Null),
    }
}

#[tokio::test]
async fn a_group_named_from_a_minted_identifier_holds_the_children_the_stack_declares() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let extra = acknowledging_iam(&[]);
        let (status, body) = create_stack(&router, row.stack, &template_of(&row), &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {body}"
        );
        let described = poll_to_terminal(&router, case, row.stack).await;
        let reported = xml_text(&described, "StackStatus");
        assert_eq!(
            reported.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {reported:?} with \
             the resource failures {:?}",
            failure_reasons(&router, row.stack).await
        );
        let published = output_value(&described, LOG_GROUP_NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish its log-group name, got {described}")
        });

        // The identifier the name is built around, as the stack publishes it, and the proof that
        // it is the one the owning service assigned rather than a stand-in.
        let identifier = output_value(&described, IDENTIFIER_OUTPUT).unwrap_or_default();
        if row.minter != Minter::None {
            let (status, answered) =
                owning_service_holds(&router, row.minter, &identifier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the service that minted {identifier:?} to hold the \
                 resource under it, got {status} with {answered}"
            );
        }
        let expected = expected_name(&row, &identifier);
        assert_eq!(
            published, expected,
            "case {case:?}: expected the stack to publish the log group named {expected:?} — the \
             name its declaration states for the identifier in force — got {published:?}"
        );

        let group = described_group(&router, &published).await;
        assert!(
            group.is_some(),
            "case {case:?}: expected CloudWatch Logs to hold a group named {published:?} — the \
             name the stack publishes — got {group:?}"
        );
        let retention = group
            .as_ref()
            .and_then(|group| group.get("retentionInDays"))
            .and_then(Value::as_i64);
        assert_eq!(
            retention,
            Some(row.retention),
            "case {case:?}: expected the group {published:?} to carry the declared retention, \
             got {retention:?}"
        );

        if row.metric_filter {
            let held = metric_filter_names(&router, &published).await;
            assert_eq!(
                held,
                vec![format!("{}-requests", row.stack)],
                "case {case:?}: expected the metric filter the template declares on \
                 {published:?}, got {held:?}"
            );
        }
        if row.subscription_filter {
            let held = subscription_filter_names(&router, &published).await;
            assert_eq!(
                held,
                vec![format!("{}-deliver", row.stack)],
                "case {case:?}: expected the subscription filter the template declares on \
                 {published:?}, got {held:?}"
            );
        }
        if row.log_stream {
            let held = stream_names(&router, &published).await;
            assert_eq!(
                held,
                vec![format!("{}-stream", row.stack)],
                "case {case:?}: expected the stream the template declares inside {published:?}, \
                 got {held:?}"
            );
        }
    }
}
