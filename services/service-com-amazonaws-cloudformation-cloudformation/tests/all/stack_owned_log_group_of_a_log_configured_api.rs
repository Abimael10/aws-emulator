// SPDX-License-Identifier: CC0-1.0

//! A stack that declares a resource whose log delivery is CONFIGURED together with the very log
//! group that delivery publishes into.
//!
//! RULE under test: **configuring a service's log delivery into CloudWatch Logs creates no log
//! group of the account's — a delivery setting names the role the delivering service publishes
//! under and the level it captures, not a group — so the group comes into existence only when
//! something creates it (the first record delivered into it, or a `CreateLogGroup` a client or a
//! template makes), a template that declares the delivering resource TOGETHER with that
//! resource's own group creates the group exactly once and completes, and `CreateLogGroup`
//! answers `ResourceAlreadyExistsException` only for a name the account already holds.**
//!
//! The reported case was a stack that created an `AWS::AppSync::GraphQLApi` with a `LogConfig`
//! and declared the API's own request log group — `/aws/appsync/apis/<apiId>`, resolved from the
//! API's `ApiId` attribute — beside a metric filter over it and an alarm over that filter's
//! metric, which is the documented way to own that group at deploy time: the group resource was
//! refused as already existing and the whole stack rolled back, so every leg behind the deploy
//! was unreachable.
//!
//! The rows differ along what the rule generalises over rather than restating that template:
//!
//! * the LEVEL the delivery is configured at (`ALL`, `ERROR`, `NONE`) and a control row that
//!   configures no delivery at all, which no row's group depends on;
//! * WHETHER the template declares the group — the row that declares none is what says
//!   configuring the delivery created nothing;
//! * WHICH children of the group the template declares (a metric filter, and an alarm over the
//!   metric that filter publishes);
//! * the retention the declaration states, one of the values the type publishes.
//!
//! Every case drives the real request path for all four services through one router — the
//! CloudFormation calls over its awsQuery wire, the AppSync read over `restJson1`, the
//! CloudWatch Logs and CloudWatch calls over their awsJson wires — so what is asserted is what a
//! client that deploys the template and then reads the resources back observes. Nothing is
//! waited out: each stack is polled to its terminal status.
//!
//! The second case is the other half of that rule, over a REDEPLOY: a deployment of a stack that
//! ALREADY holds the minting resource resolves the declaration against the identities in force,
//! so a template that states the group again configures the group it created rather than creating
//! one under a name composed from a stand-in and taking the real one down with it. Its rows
//! differ along WHICH service minted the identifier (AWS AppSync's api id through an
//! `Fn::GetAtt`, Amazon API Gateway v2's through a `Ref`), HOW the name composes it (at the end
//! of the name, and in the middle of a longer one) and the retention each deployment states. The
//! converse — a declaration of a name the account holds OUTSIDE the stack, which
//! CloudFormation's pre-deployment existence check refuses before anything is provisioned — is
//! the subject of `implicit_log_group_collision.rs`.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_event_and_merged_api_resources::appsync;
use crate::declared_alarm_is_readable_in_cloudwatch::alarms_named;
use crate::dynamodb_table_resources::failure_reasons;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::logs_and_iot_resources::described_group;
use crate::logs_group_children_resources::logs;
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, member, output_value, router, xml_text,
};

/// The prefix AWS AppSync publishes an API's own request logs under, which is the prefix a
/// template composing that group's name writes before the api id.
const APPSYNC_LOG_GROUP_PREFIX: &str = "/aws/appsync/apis/";

/// The namespace the metric a declared filter publishes lives in, and that metric's own name.
const METRIC_NAMESPACE: &str = "Sim/GraphQL";
const METRIC_NAME: &str = "GraphQLRequests";

/// The logical ids the templates declare.
const API: &str = "Api";
const LOGS_ROLE: &str = "ApiLogsRole";
const GROUP: &str = "ApiLogs";
const FILTER: &str = "ApiLogsFilter";
const ALARM: &str = "ApiLogsAlarm";

/// The outputs the templates publish: the api id the group's name is composed from, the group's
/// own name, and the alarm's name where a row declares one.
const API_ID_OUTPUT: &str = "ApiId";
const LOG_GROUP_NAME_OUTPUT: &str = "LogGroupName";
const ALARM_NAME_OUTPUT: &str = "AlarmName";

/// The fixed sentence CloudWatch Logs answers a create for a name it already holds with, as
/// `CreateLogGroup`'s `ResourceAlreadyExistsException` words it.
const ALREADY_EXISTS_MESSAGE: &str = "The specified log group already exists";

/// The error code that refusal is answered under.
const ALREADY_EXISTS_CODE: &str = "ResourceAlreadyExistsException";

/// One row: what the template declares around the API's own log group.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `FieldLogLevel` the API's `LogConfig` states, or `None` for an API that configures no
    /// delivery at all. Source: `LogConfig$fieldLogLevel`'s own values
    /// (`smithy_query(query="shape",shape="FieldLogLevel")`).
    level: Option<&'static str>,
    /// Whether the template declares the API's own request log group.
    declares_group: bool,
    /// Whether it declares an `AWS::Logs::MetricFilter` over that group — the documented reason a
    /// template owns the group at deploy time.
    metric_filter: bool,
    /// Whether it declares an `AWS::CloudWatch::Alarm` over the metric that filter publishes.
    alarm: bool,
    /// The retention the group's declaration states, a value `RetentionInDays` publishes.
    retention: i64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an API logging every field, with the group it logs into and a metric filter \
                   over it",
            stack: "graphql-logs-all",
            level: Some("ALL"),
            declares_group: true,
            metric_filter: true,
            alarm: false,
            retention: 1,
        },
        Row {
            case: "the reported shape: an API logging every field, its own group, the metric \
                   filter over it and the alarm over that filter's metric",
            stack: "graphql-logs-alarmed",
            level: Some("ALL"),
            declares_group: true,
            metric_filter: true,
            alarm: true,
            retention: 7,
        },
        Row {
            case: "an API logging the fields in error, with its own group and nothing inside it",
            stack: "graphql-logs-error",
            level: Some("ERROR"),
            declares_group: true,
            metric_filter: false,
            alarm: false,
            retention: 30,
        },
        Row {
            case: "an API logging no field, whose request-level records still go to the group \
                   the stack owns",
            stack: "graphql-logs-none",
            level: Some("NONE"),
            declares_group: true,
            metric_filter: true,
            alarm: false,
            retention: 90,
        },
        Row {
            case: "an API logging every field whose template declares no group, which must \
                   leave the account holding none",
            stack: "graphql-logs-unowned",
            level: Some("ALL"),
            declares_group: false,
            metric_filter: false,
            alarm: false,
            retention: 1,
        },
        Row {
            case: "the control: an API that configures no delivery, with the group under its \
                   name declared anyway",
            stack: "graphql-logs-absent",
            level: None,
            declares_group: true,
            metric_filter: true,
            alarm: false,
            retention: 1,
        },
    ]
}

/// The template one row deploys.
fn template_of(row: &Row) -> String {
    let mut resources = Map::new();
    let mut api_properties = json!({
        "Name": format!("{}-api", row.stack),
        "AuthenticationType": "API_KEY",
    });
    if let Some(level) = row.level {
        // The role the delivery is made under, which is what a `LogConfig` names besides the
        // level — and which creates no group either.
        resources.insert(
            LOGS_ROLE.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-logs", row.stack),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "appsync.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            }),
        );
        api_properties["LogConfig"] = json!({
            "FieldLogLevel": level,
            "CloudWatchLogsRoleArn": {"Fn::GetAtt": [LOGS_ROLE, "Arn"]},
            "ExcludeVerboseContent": false,
        });
    }
    resources.insert(
        API.to_string(),
        json!({"Type": "AWS::AppSync::GraphQLApi", "Properties": api_properties}),
    );
    let mut outputs = json!({API_ID_OUTPUT: {"Value": {"Fn::GetAtt": [API, "ApiId"]}}});
    if row.declares_group {
        resources.insert(
            GROUP.to_string(),
            json!({
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": {
                        "Fn::Join": [
                            "",
                            [APPSYNC_LOG_GROUP_PREFIX, {"Fn::GetAtt": [API, "ApiId"]}],
                        ],
                    },
                    "RetentionInDays": row.retention,
                },
            }),
        );
        outputs[LOG_GROUP_NAME_OUTPUT] = json!({"Value": {"Ref": GROUP}});
    }
    if row.metric_filter {
        resources.insert(
            FILTER.to_string(),
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
    if row.alarm {
        resources.insert(
            ALARM.to_string(),
            json!({
                "Type": "AWS::CloudWatch::Alarm",
                "DependsOn": FILTER,
                "Properties": {
                    "AlarmName": format!("{}-requests", row.stack),
                    "Namespace": METRIC_NAMESPACE,
                    "MetricName": METRIC_NAME,
                    "Statistic": "Sum",
                    "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                    "Period": 60,
                    "EvaluationPeriods": 1,
                    "Threshold": 1,
                },
            }),
        );
        outputs[ALARM_NAME_OUTPUT] = json!({"Value": {"Ref": ALARM}});
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// The names of the metric filters CloudWatch Logs holds on `group`.
async fn metric_filter_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, document) = logs(
        router,
        "DescribeMetricFilters",
        &json!({"logGroupName": group}),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    document
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
        .unwrap_or_default()
}

/// The `logConfig` AWS AppSync itself reports for the API `api_id`, as a client reading the API
/// back sees it.
async fn reported_log_config(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> Option<Value> {
    let (status, document) = appsync(router, &format!("/v1/apis/{api_id}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AppSync to hold the API it minted {api_id:?}, got {status} \
         with {document}"
    );
    document
        .get("graphqlApi")
        .and_then(|api| api.get("logConfig"))
        .filter(|config| !config.is_null())
        .cloned()
}

#[tokio::test]
async fn a_log_configured_api_leaves_its_group_to_the_stack_that_declares_it() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let (status, body) = create_stack(
            &router,
            row.stack,
            &template_of(&row),
            &acknowledging_iam(&[]),
        )
        .await;
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

        // The api id the group's name is composed from, and the proof it is the one AppSync
        // assigned: the level the declaration configured is the level that service reports.
        let api_id = output_value(&described, API_ID_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the api id, got {described}")
        });
        let log_config = reported_log_config(&router, case, &api_id).await;
        assert_eq!(
            log_config
                .as_ref()
                .and_then(|config| config.get("fieldLogLevel"))
                .and_then(Value::as_str),
            row.level,
            "case {case:?}: expected AppSync to report the declared level, got {log_config:?}"
        );

        let group = format!("{APPSYNC_LOG_GROUP_PREFIX}{api_id}");
        if !row.declares_group {
            // Nothing created the group: configuring the delivery names a role and a level,
            // neither of which is a group of the account's.
            let held = described_group(&router, &group).await;
            assert!(
                held.is_none(),
                "case {case:?}: expected CloudWatch Logs to hold no group {group:?} for an API \
                 whose template declares none, got {held:?}"
            );
            continue;
        }

        let published = output_value(&described, LOG_GROUP_NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish its log-group name, got {described}")
        });
        assert_eq!(
            published, group,
            "case {case:?}: expected the stack to publish the group named for the api id in \
             force, got {published:?}"
        );
        let held = described_group(&router, &published).await;
        assert!(
            held.is_some(),
            "case {case:?}: expected CloudWatch Logs to hold the group {published:?} the stack \
             created, got {held:?}"
        );
        let retention = held
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
            let filters = metric_filter_names(&router, &published).await;
            assert_eq!(
                filters,
                vec![format!("{}-requests", row.stack)],
                "case {case:?}: expected the metric filter the template declares on \
                 {published:?}, got {filters:?}"
            );
        }
        if row.alarm {
            let name = output_value(&described, ALARM_NAME_OUTPUT).unwrap_or_else(|| {
                panic!("case {case:?}: the stack must publish its alarm name, got {described}")
            });
            let held = alarms_named(&router, case, &name).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected CloudWatch to hold the alarm {name:?} over the \
                 filter's metric, got {held:?}"
            );
        }

        // The group now exists, and it exists ONCE: a second create of that same name — the
        // only case the refusal is owed — is answered with it.
        let (status, refused) = logs(
            &router,
            "CreateLogGroup",
            &json!({"logGroupName": published}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a create of the name the stack holds to be refused 400, \
             got {status} with {refused}"
        );
        assert_eq!(
            refused["__type"]
                .as_str()
                .unwrap_or_default()
                .rsplit('#')
                .next(),
            Some(ALREADY_EXISTS_CODE),
            "case {case:?}: expected {ALREADY_EXISTS_CODE}, got {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(ALREADY_EXISTS_MESSAGE),
            "case {case:?}: expected the sentence that refusal states, got {refused}"
        );
    }
}

/// One row of the table above, at the retention a case states: the estate of the API that logs
/// into the group its own stack owns.
fn log_configured_row(case: &'static str, stack: &'static str, retention: i64) -> Row {
    Row {
        case,
        stack,
        level: Some("ALL"),
        declares_group: true,
        metric_filter: true,
        alarm: false,
        retention,
    }
}

/// Which service mints the identifier a redeployed group's name is composed out of, and how the
/// declaration reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Minter {
    /// AWS AppSync, which assigns a GraphQL API's `ApiId` — read through `Fn::GetAtt`, with the
    /// identifier at the END of the name, on the API that configures its own log delivery.
    AppSyncApiId,
    /// Amazon API Gateway v2, which assigns an api's id the same way ("Ref returns the API ID")
    /// — read through a `Ref`, with the identifier in the MIDDLE of a longer name.
    ApiGatewayV2ApiId,
}

/// One redeploy row: the estate the template declares, the name its group carries around the
/// identifier the minting service assigned, and the retentions the two deployments state.
struct Redeploy {
    case: &'static str,
    stack: &'static str,
    minter: Minter,
    /// What the group's name begins with, and what follows the identifier in it.
    prefix: &'static str,
    suffix: &'static str,
    /// The `RetentionInDays` the create states, then the one the update states.
    retentions: (i64, i64),
}

fn redeploys() -> Vec<Redeploy> {
    vec![
        Redeploy {
            case: "the reported shape redeployed: an API logging every field, its own request \
                   log group and the metric filter over it",
            stack: "graphql-logs-updated",
            minter: Minter::AppSyncApiId,
            prefix: APPSYNC_LOG_GROUP_PREFIX,
            suffix: "",
            retentions: (1, 7),
        },
        Redeploy {
            case: "a group named after the api id API Gateway v2 minted, read with a Ref inside \
                   a longer name",
            stack: "httpapi-logs-updated",
            minter: Minter::ApiGatewayV2ApiId,
            prefix: "/aws/apigateway/http-apis/",
            suffix: "/requests",
            retentions: (30, 90),
        },
    ]
}

/// The template one redeploy row deploys at `retention`.
fn redeploy_template(row: &Redeploy, retention: i64) -> String {
    match row.minter {
        Minter::AppSyncApiId => template_of(&log_configured_row(row.case, row.stack, retention)),
        Minter::ApiGatewayV2ApiId => http_api_template(row, retention),
    }
}

/// The logical id of the api an API Gateway v2 row declares.
const HTTP_API: &str = "HttpApi";

/// An HTTP api, the log group whose name is composed around the id that service minted for it,
/// and the metric filter over that group.
fn http_api_template(row: &Redeploy, retention: i64) -> String {
    let identifier = json!({"Ref": HTTP_API});
    json!({
        "Resources": {
            HTTP_API: {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": format!("{}-api", row.stack),
                    "ProtocolType": "HTTP",
                },
            },
            GROUP: {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": {
                        "Fn::Sub": [
                            format!("{}${{Identifier}}{}", row.prefix, row.suffix),
                            {"Identifier": identifier},
                        ],
                    },
                    "RetentionInDays": retention,
                },
            },
            FILTER: {
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
            },
        },
        "Outputs": {
            API_ID_OUTPUT: {"Value": identifier},
            LOG_GROUP_NAME_OUTPUT: {"Value": {"Ref": GROUP}},
        },
    })
    .to_string()
}

#[tokio::test]
async fn a_redeploy_configures_the_group_it_created_rather_than_renaming_it() {
    // The other half of the rule: a deployment of a stack that ALREADY holds the minting
    // resource resolves the group's declaration against the identity IN FORCE, so the group it
    // states again is the group it created — configured, not created under a second name.
    for row in redeploys() {
        let case = row.case;
        let router = router();
        let (status, body) = create_stack(
            &router,
            row.stack,
            &redeploy_template(&row, row.retentions.0),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {body}"
        );
        let described = poll_to_terminal(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described} \
             because {:?}",
            failure_reasons(&router, row.stack).await
        );
        let identifier = output_value(&described, API_ID_OUTPUT).unwrap_or_default();
        let published = output_value(&described, LOG_GROUP_NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish its log-group name, got {described}")
        });
        assert_eq!(
            published,
            format!("{}{identifier}{}", row.prefix, row.suffix),
            "case {case:?}: expected the group named for the identifier {identifier:?} the \
             minting service assigned, got {published:?}"
        );
        let before = described_group(&router, &published)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: CloudWatch Logs must hold {published:?}"));

        let mut members = vec![
            member("StackName", row.stack),
            member(
                "TemplateBody",
                &redeploy_template(&row, row.retentions.1),
            ),
        ];
        members.extend(acknowledging_iam(&[]));
        let (status, body) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the in-place update must be accepted, got {status} {body}"
        );
        let redescribed = poll_to_terminal(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&redescribed, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {redescribed} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The same group is still the stack's: the name it publishes is the name the create
        // made, around the identifier that is still in force.
        assert_eq!(
            output_value(&redescribed, API_ID_OUTPUT).as_deref(),
            Some(identifier.as_str()),
            "case {case:?}: expected the identifier to be unchanged by the update, got \
             {redescribed}"
        );
        let republished = output_value(&redescribed, LOG_GROUP_NAME_OUTPUT);
        assert_eq!(
            republished.as_deref(),
            Some(published.as_str()),
            "case {case:?}: expected the update to publish the group the create made, got \
             {republished:?}"
        );
        let after = described_group(&router, &published)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected CloudWatch Logs to still hold {published:?} after \
                     the update"
                )
            });
        assert_eq!(
            after.get("retentionInDays").and_then(Value::as_i64),
            Some(row.retentions.1),
            "case {case:?}: expected the retention now declared on {published:?}, got {after:?}"
        );
        // The same group throughout: a group taken away and made again would carry another
        // creation instant, and its ARN is the identity a reader of the stack's output
        // resolves.
        for member_name in ["creationTime", "arn"] {
            assert_eq!(
                after.get(member_name),
                before.get(member_name),
                "case {case:?}: expected the update to configure the group the create made, \
                 whose {member_name} is {:?}, got {after:?}",
                before.get(member_name)
            );
        }
        // ...and the child the template declares inside it is still on that group.
        let filters = metric_filter_names(&router, &published).await;
        assert_eq!(
            filters,
            vec![format!("{}-requests", row.stack)],
            "case {case:?}: expected the metric filter to stay on {published:?}, got {filters:?}"
        );
    }
}
