// SPDX-License-Identifier: CC0-1.0

//! The three CloudWatch Application Signals resources a template declares, end to end: each
//! exists in that service itself under the handle the stack publishes, a changed declaration
//! reaches it, deleting the stack takes it away — and a declaration that service REFUSES fails
//! the resource with its own refusal and rolls the stack back.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may declare, and a
//! declaration of it is DEPLOYED by calling the service that owns the type — so the handle the
//! stack publishes (`Ref`) resolves in that service, the values the type publishes are the ones
//! that service reported, and what that service refuses is a FAILED RESOURCE rather than the
//! `Template format error: Unrecognized resource types` a type CloudFormation does not know is
//! refused with, which no row of any table here may ever see.
//!
//! The rows of each table differ along what the rule generalises over: WHICH of the three types
//! the template declares, which of each type's shapes the declaration uses (a period-based
//! objective against a request-based one, a code location in each programming language the
//! model publishes), how many resources one stack declares, whether the declaration carries the
//! type's own `Tags` and the optional members its create takes, and which published return
//! value an output reads.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds
//! — the CloudFormation calls over its awsQuery wire, the Application Signals calls over that
//! service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls Application Signals observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, failed_resource_reason, member, quoted_and_class, router,
    settled_stack_status, stack_event_reasons, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value};

/// The three resource types under test.
const SLO: &str = "AWS::ApplicationSignals::ServiceLevelObjective";
const GROUPING_CONFIGURATION: &str = "AWS::ApplicationSignals::GroupingConfiguration";
const INSTRUMENTATION_CONFIG: &str = "AWS::ApplicationSignals::InstrumentationConfig";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of any table here may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The error code Application Signals answers for a resource it does not hold, and the status
/// that error carries (the modelled 404 of `GetServiceLevelObjective` and of
/// `GetInstrumentationConfiguration`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The one `DynamicInstrumentationSignalType` the model declares
/// (`smithy_query(query="shape",shape="DynamicInstrumentationSignalType")` -> `["SNAPSHOT"]`).
const SNAPSHOT: &str = "SNAPSHOT";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// One Application Signals call over the `restJson1` wire its model declares, on the method and
/// URI that operation's own `@http` binding names, signed for `application-signals` — that
/// model's own `sigv4Name` — in the Region the stack's resources live in.
async fn signals(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://application-signals.{REGION}.amazonaws.com{uri}"
        ))
        .header("host", format!("application-signals.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/application-signals/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Application Signals request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of ONE objective, by the ARN the stack published — the `Id` every
/// operation on an objective takes, escaped as a single path segment.
async fn get_slo(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    signals(router, "GET", &format!("/slo/{}", form_encode(id)), None).await
}

/// The exclusion windows that service holds for one objective.
async fn exclusion_windows(router: &MultiServiceRouter, id: &str) -> Vec<Value> {
    let (status, answered) = signals(
        router,
        "GET",
        &format!("/slo/{}/exclusion-windows", form_encode(id)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the exclusion windows of {id} to be readable, got {status} with {answered}"
    );
    answered
        .get("ExclusionWindows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one resource, by its ARN.
async fn tags_of(router: &MultiServiceRouter, arn: &str) -> Vec<(String, String)> {
    let (status, answered) = signals(
        router,
        "GET",
        &format!("/tags?ResourceArn={}", form_encode(arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the tags of {arn} to be readable, got {status} with {answered}"
    );
    answered
        .get("Tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("Key").and_then(Value::as_str)?.to_string(),
                        tag.get("Value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The account's grouping attribute definitions, as that service's own read reports them,
/// beside the instant it recorded for the configuration.
async fn grouping_attribute_definitions(router: &MultiServiceRouter) -> (Vec<Value>, Value) {
    let (status, answered) = signals(
        router,
        "POST",
        "/grouping-attribute-definitions",
        Some(&json!({})),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the grouping attribute definitions to be readable, got {status} with {answered}"
    );
    (
        answered
            .get("GroupingAttributeDefinitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        answered.get("UpdatedAt").cloned().unwrap_or(Value::Null),
    )
}

/// That service's own read of ONE instrumentation configuration, by the identity the stack
/// declared and the location hash it published.
async fn get_instrumentation_configuration(
    router: &MultiServiceRouter,
    service: &str,
    environment: &str,
    instrumentation_type: &str,
    location_hash: &str,
) -> (u16, Value) {
    signals(
        router,
        "POST",
        "/get-instrumentation-configuration",
        Some(&json!({
            "InstrumentationType": instrumentation_type,
            "Service": service,
            "Environment": environment,
            "SignalType": SNAPSHOT,
            "LocationIdentifier": { "LocationHash": location_hash },
        })),
    )
    .await
}

/// Submit `template` as `stack_name`, carrying the stack's own tag, and answer the `CreateStack`
/// body: a type Amazon Web Services publishes is ADMITTED, so this never sees the
/// template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    body
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// The value of one output of a described stack, which every row of this suite expects to be
/// there: an output carrying a key and no value is what a client's `{o["OutputKey"]:
/// o["OutputValue"]}` read raises on.
fn output(case: &str, described: &str, key: &str) -> String {
    output_value(described, key).unwrap_or_else(|| {
        panic!("case {case:?}: expected an {key} output carrying a value, got {described}")
    })
}

/// Delete `stack_name` and wait until this service no longer reports it.
async fn deleted(router: &MultiServiceRouter, case: &str, stack_name: &str) {
    let (status, body) = call(router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = describe_stack(router, stack_name).await;
        if status != 200 || described.contains("DELETE_COMPLETE") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected {stack_name} to finish deleting, it reported {described}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// A `CaptureConfiguration` with only its `@required` members
/// (`smithy_query(query="shape",shape="CodeCaptureConfiguration,CaptureLimitsConfig")`).
fn minimal_capture() -> Value {
    json!({ "CodeCapture": { "CaptureLimits": { "MaxHits": 5 } } })
}

/// RULE, admission half: each of the three types Application Signals publishes is a type a
/// template may declare, and a NEAR MISS of one — a namespace that publishes no such type, a
/// resource name that service has none of, another case — is refused `Template format error:
/// Unrecognized resource types: [<types>]` with nothing created.
///
/// The rows span the three published types and the forms a near miss takes.
#[tokio::test]
async fn each_published_type_is_admitted_and_a_near_miss_is_not() {
    let router = router();
    let published: &[(&str, &str, Value)] = &[
        (
            "the service level objective",
            SLO,
            json!({
                "Name": "admitted-objective",
                "Sli": {
                    "SliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "checkout",
                            "Environment": "eks:cluster-prod/namespace",
                        },
                        "MetricType": "LATENCY",
                        "PeriodSeconds": 60,
                        "Statistic": "p99",
                    },
                    "MetricThreshold": 200.0,
                    "ComparisonOperator": "LessThan",
                },
            }),
        ),
        (
            "the account's grouping configuration",
            GROUPING_CONFIGURATION,
            json!({
                "GroupingAttributeDefinitions": [
                    { "GroupingName": "BusinessUnit", "GroupingSourceKeys": ["business.unit"] },
                ],
            }),
        ),
        (
            "the instrumentation config",
            INSTRUMENTATION_CONFIG,
            json!({
                "InstrumentationType": "PROBE",
                "Service": "admitted-service",
                "Environment": "ec2:production",
                "SignalType": SNAPSHOT,
                "Location": {
                    "CodeLocation": {
                        "Language": "Java",
                        "CodeUnit": "com.example.checkout",
                        "ClassName": "CartService",
                        "MethodName": "total",
                        "FilePath": "src/main/java/com/example/CartService.java",
                    },
                },
                "CaptureConfiguration": minimal_capture(),
            }),
        ),
    ];

    for (case, resource_type, properties) in published {
        let stack_name = format!("appsig-admitted-{}", resource_type.replace("::", "-"));
        let template = json!({
            "Resources": { "Declared": { "Type": resource_type, "Properties": properties } }
        })
        .to_string();
        submitted(&router, case, &stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, &stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected a declaration of a published type to deploy, got {reached} \
             with failure reasons {:?}",
            stack_event_reasons(&router, REGION, &stack_name).await
        );
    }

    // A near miss of one of them is not a type at all: the template is refused, and nothing is
    // created.
    for (index, spelled) in [
        "AWS::ApplicationSignals::Slo",
        "AWS::ApplicationSignals::ServiceLevelObjectives",
        "AWS::ApplicationSignals::InstrumentationConfiguration",
        "AWS::ApplicationSignals::groupingConfiguration",
        "AWS::ApplicationSignal::ServiceLevelObjective",
    ]
    .iter()
    .enumerate()
    {
        let stack_name = format!("appsig-near-miss-{index}");
        let template = json!({
            "Resources": { "Declared": { "Type": spelled, "Properties": {} } }
        })
        .to_string();
        let (status, body) = create_stack(&router, &stack_name, &template, &[]).await;
        assert_eq!(
            status, 400,
            "case {spelled:?}: expected a near miss of a published type to be refused, got \
             {status} with {body}"
        );
        assert!(
            body.contains(&format!(
                "Template format error: Unrecognized resource types: [{spelled}]"
            )),
            "case {spelled:?}: expected the unrecognized-types refusal naming it, got {body}"
        );
        let (status, described) = describe_stack(&router, &stack_name).await;
        assert_ne!(
            status, 200,
            "case {spelled:?}: expected the refused template to have created no stack, got \
             {described}"
        );
    }
}

/// RULE, provisioning half, for the OBJECTIVE: the objective the template declares is created
/// in Application Signals under the ARN the stack publishes, carries the configuration and the
/// tags the declaration asked for beside the stack's own, is applied in place when the
/// declaration changes, and is gone from that service once the stack is deleted.
///
/// The rows differ along which of the two indicators the objective is defined by (a
/// period-based `Sli` and a request-based `RequestBasedSli`, which is what settles its
/// `EvaluationType`), whether the declaration carries a description, a goal, the type's own
/// `Tags` and an exclusion window, and which published return value the outputs read.
#[tokio::test]
async fn a_declared_objective_is_created_read_back_and_removed_with_the_stack() {
    struct Row {
        case: &'static str,
        stack_name: &'static str,
        objective: &'static str,
        properties: Value,
        evaluation_type: &'static str,
        tags: &'static [(&'static str, &'static str)],
        exclusion_windows: usize,
    }

    let rows = vec![
        Row {
            case: "a period-based objective with a goal, tags and an exclusion window",
            stack_name: "appsig-slo-period-based",
            objective: "cfn-latency-objective",
            properties: json!({
                "Name": "cfn-latency-objective",
                "Description": "the checkout latency objective",
                "Sli": {
                    "SliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "checkout",
                            "Environment": "eks:cluster-prod/namespace",
                        },
                        "OperationName": "GET /cart",
                        "MetricType": "LATENCY",
                        "PeriodSeconds": 300,
                        "Statistic": "p99",
                    },
                    "MetricThreshold": 250.0,
                    "ComparisonOperator": "LessThanOrEqualTo",
                },
                "Goal": {
                    "AttainmentGoal": 99.5,
                    "WarningThreshold": 40.0,
                    "Interval": { "RollingInterval": { "DurationUnit": "DAY", "Duration": 7 } },
                },
                "ExclusionWindows": [{
                    "Window": { "DurationUnit": "HOUR", "Duration": 2 },
                    "StartTime": "2026-01-01T00:00:00Z",
                    "Reason": "the monthly maintenance window",
                }],
                "Tags": [{ "Key": "team", "Value": "checkout" }],
            }),
            evaluation_type: "PeriodBased",
            tags: &[("team", "checkout")],
            exclusion_windows: 1,
        },
        Row {
            case: "a request-based objective with nothing optional declared",
            stack_name: "appsig-slo-request-based",
            objective: "cfn-availability-objective",
            properties: json!({
                "Name": "cfn-availability-objective",
                "RequestBasedSli": {
                    "RequestBasedSliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "pricing",
                            "Environment": "ec2:production",
                        },
                        "MetricType": "AVAILABILITY",
                    },
                },
            }),
            evaluation_type: "RequestBased",
            tags: &[],
            exclusion_windows: 0,
        },
    ];

    for row in rows {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": { "Objective": { "Type": SLO, "Properties": row.properties } },
            "Outputs": {
                "ObjectiveRef": { "Value": { "Ref": "Objective" } },
                "ObjectiveArn": { "Value": { "Fn::GetAtt": ["Objective", "Arn"] } },
                "CreatedTime": { "Value": { "Fn::GetAtt": ["Objective", "CreatedTime"] } },
                "LastUpdatedTime": { "Value": { "Fn::GetAtt": ["Objective", "LastUpdatedTime"] } },
                "EvaluationType": { "Value": { "Fn::GetAtt": ["Objective", "EvaluationType"] } },
            }
        })
        .to_string();

        let described = deployed(&router, case, row.stack_name, &template).await;
        let published = output(case, &described, "ObjectiveRef");
        assert_eq!(
            published,
            output(case, &described, "ObjectiveArn"),
            "case {case:?}: expected Ref to return the ARN the type publishes, got {published}"
        );
        assert!(
            published.ends_with(&format!(":slo/{}", row.objective)),
            "case {case:?}: expected the ARN to carry the objective's name, got {published}"
        );

        // The objective really exists in Application Signals, under the handle the stack
        // published, with the configuration the declaration asked for.
        let (status, read) = get_slo(&router, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Application Signals to report the objective the stack \
             created, got {status} with {read}"
        );
        let objective = &read["Slo"];
        assert_eq!(
            objective["Name"].as_str(),
            Some(row.objective),
            "case {case:?}: expected the objective to carry the declared name, got {read}"
        );
        assert_eq!(
            objective["EvaluationType"].as_str(),
            Some(row.evaluation_type),
            "case {case:?}: expected the indicator the declaration states to settle the \
             evaluation type, got {read}"
        );
        assert_eq!(
            objective["Arn"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the stored objective to carry the ARN the stack published, \
             got {read}"
        );

        // The values the type publishes are the ones that service reported, not stand-ins.
        for (key, member) in [
            ("CreatedTime", "CreatedTime"),
            ("LastUpdatedTime", "LastUpdatedTime"),
            ("EvaluationType", "EvaluationType"),
        ] {
            let reported = output(case, &described, key);
            assert!(
                !reported.contains("is not available"),
                "case {case:?}: expected {key} to be the value that service reported, got \
                 {reported}"
            );
            if member == "EvaluationType" {
                assert_eq!(
                    reported, row.evaluation_type,
                    "case {case:?}: expected the reported evaluation type, got {reported}"
                );
            }
        }

        // The tags the declaration asked for reach that service beside the stack's own.
        let held = tags_of(&router, &published).await;
        for (key, value) in row.tags {
            assert!(
                held.contains(&((*key).to_string(), (*value).to_string())),
                "case {case:?}: expected the declared tag {key:?} on the objective, got {held:?}"
            );
        }
        assert!(
            held.iter()
                .any(|(key, value)| key == STACK_TAG_KEY && value == row.stack_name),
            "case {case:?}: expected the stack's own tag to be propagated, got {held:?}"
        );

        // The exclusion windows the declaration states are written to the objective itself.
        let windows = exclusion_windows(&router, &published).await;
        assert_eq!(
            windows.len(),
            row.exclusion_windows,
            "case {case:?}: expected the declared exclusion windows on the objective, got \
             {windows:?}"
        );

        deleted(&router, case, row.stack_name).await;
        let (status, gone) = get_slo(&router, &published).await;
        assert_eq!(
            (status, gone.get("__type").and_then(Value::as_str)),
            (NOT_FOUND_STATUS, Some(RESOURCE_NOT_FOUND)),
            "case {case:?}: expected the objective to be gone with the stack, got {status} with \
             {gone}"
        );
    }
}

/// RULE, provisioning half, for the OBJECTIVE's update: `UpdateServiceLevelObjective` takes a
/// new configuration for the objective that exists, so a changed declaration is applied to the
/// objective the stack already has — under the same ARN — rather than being dropped.
#[tokio::test]
async fn a_changed_objective_declaration_is_applied_in_place() {
    let router = router();
    let case = "an update that restates the description and the threshold";
    let stack_name = "appsig-slo-updated";
    let template_of = |description: &str, threshold: f64| {
        json!({
            "Resources": {
                "Objective": {
                    "Type": SLO,
                    "Properties": {
                        "Name": "cfn-updated-objective",
                        "Description": description,
                        "Sli": {
                            "SliMetric": {
                                "KeyAttributes": {
                                    "Type": "Service",
                                    "Name": "checkout",
                                    "Environment": "ec2:production",
                                },
                                "MetricType": "LATENCY",
                                "PeriodSeconds": 60,
                                "Statistic": "p99",
                            },
                            "MetricThreshold": threshold,
                            "ComparisonOperator": "LessThan",
                        },
                    }
                }
            },
            "Outputs": { "ObjectiveRef": { "Value": { "Ref": "Objective" } } }
        })
        .to_string()
    };

    let described = deployed(&router, case, stack_name, &template_of("the first", 100.0)).await;
    let published = output(case, &described, "ObjectiveRef");

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of("the second", 500.0)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    let (status, read) = get_slo(&router, &published).await;
    assert_eq!(
        (
            status,
            read["Slo"]["Description"].as_str(),
            read["Slo"]["Sli"]["MetricThreshold"].as_f64(),
        ),
        (200, Some("the second"), Some(500.0)),
        "case {case:?}: expected the changed declaration to reach the objective the stack has, \
         got {status} with {read}"
    );
}

/// RULE, provisioning half, for the GROUPING CONFIGURATION: the account holds ONE configuration
/// ("Creates or updates the grouping configuration for this account"), so the definitions the
/// template declares are the ones that service reports, a changed declaration overwrites them
/// in place under the same identity, and deleting the stack removes them.
#[tokio::test]
async fn a_declared_grouping_configuration_is_the_accounts_own_singleton() {
    let router = router();
    let case = "a grouping configuration redeclared with another definition set";
    let stack_name = "appsig-grouping-configuration";
    let template_of = |definitions: Value| {
        json!({
            "Resources": {
                "Grouping": {
                    "Type": GROUPING_CONFIGURATION,
                    "Properties": { "GroupingAttributeDefinitions": definitions }
                }
            },
            "Outputs": {
                "GroupingRef": { "Value": { "Ref": "Grouping" } },
                "AwsAccountId": { "Value": { "Fn::GetAtt": ["Grouping", "AwsAccountId"] } },
                "UpdatedAt": { "Value": { "Fn::GetAtt": ["Grouping", "UpdatedAt"] } },
            }
        })
        .to_string()
    };
    let first = json!([
        { "GroupingName": "BusinessUnit", "GroupingSourceKeys": ["business.unit"] },
        {
            "GroupingName": "Team",
            "GroupingSourceKeys": ["team.name", "owner.team"],
            "DefaultGroupingValue": "unassigned",
        },
    ]);
    let second = json!([
        { "GroupingName": "CostCentre", "DefaultGroupingValue": "shared" },
    ]);

    let described = deployed(&router, case, stack_name, &template_of(first.clone())).await;
    let published = output(case, &described, "GroupingRef");
    assert_eq!(
        published,
        output(case, &described, "AwsAccountId"),
        "case {case:?}: expected Ref to return the account whose configuration it is, got \
         {published}"
    );
    let recorded = output(case, &described, "UpdatedAt");
    assert!(
        !recorded.contains("is not available"),
        "case {case:?}: expected the instant that service recorded, got {recorded}"
    );

    let (definitions, updated_at) = grouping_attribute_definitions(&router).await;
    assert_eq!(
        Value::Array(definitions),
        first,
        "case {case:?}: expected the declared definitions to be the account's own"
    );
    assert!(
        !updated_at.is_null(),
        "case {case:?}: expected that service to record an instant for the configuration"
    );

    // One account, one configuration: the second declaration replaces the definitions rather
    // than adding a second configuration beside them.
    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of(second.clone())),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );
    let (definitions, _) = grouping_attribute_definitions(&router).await;
    assert_eq!(
        Value::Array(definitions),
        second,
        "case {case:?}: expected the redeclared definitions to overwrite the ones in force"
    );

    // The values the type publishes survive the update: the resource is the same singleton, so
    // every output still carries the value that service reported for it.
    let (status, described) = describe_stack(&router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    assert_eq!(
        output(case, &described, "GroupingRef"),
        published,
        "case {case:?}: expected the identity to survive the update"
    );
    let recorded_again = output(case, &described, "UpdatedAt");
    assert!(
        !recorded_again.contains("is not available"),
        "case {case:?}: expected the instant the update recorded, got {recorded_again}"
    );

    // Deleting the stack removes the configuration: "This removes all custom grouping attribute
    // definitions".
    deleted(&router, case, stack_name).await;
    let (definitions, _) = grouping_attribute_definitions(&router).await;
    assert!(
        definitions.is_empty(),
        "case {case:?}: expected the configuration to be gone with the stack, got {definitions:?}"
    );
}

/// RULE, provisioning half, for the INSTRUMENTATION POINT: the point the template declares is
/// recorded in Application Signals under the ARN that service minted — which carries the
/// `LocationHash` it computed — so the SDK of the instrumented service finds it, and deleting
/// the stack takes it away.
///
/// The rows differ along the programming LANGUAGE the location names (the whole modelled
/// catalogue — `smithy_query(query="shape",shape="ProgrammingLanguage")` -> `["Java", "Python",
/// "Javascript"]`), which of `CodeLocation`'s optional members it supplies, the instrumentation
/// type, and whether the declaration carries a description, attribute filters and tags.
#[tokio::test]
async fn a_declared_instrumentation_point_is_recorded_where_the_sdk_fetches_it() {
    struct Row {
        case: &'static str,
        stack_name: &'static str,
        service: &'static str,
        instrumentation_type: &'static str,
        properties: Value,
    }

    let rows = vec![
        Row {
            case: "a Java breakpoint with every optional member",
            stack_name: "appsig-instrumentation-java",
            service: "checkout",
            instrumentation_type: "BREAKPOINT",
            properties: json!({
                "InstrumentationType": "BREAKPOINT",
                "Service": "checkout",
                "Environment": "eks:cluster-prod/namespace",
                "SignalType": SNAPSHOT,
                "Description": "the discount path",
                "AttributeFilters": [{ "service.version": "1.2.3" }],
                "Location": {
                    "CodeLocation": {
                        "Language": "Java",
                        "CodeUnit": "com.example.checkout",
                        "ClassName": "CartController",
                        "MethodName": "applyDiscount",
                        "FilePath": "src/main/java/com/example/CartController.java",
                        "LineNumber": 142,
                    },
                },
                "CaptureConfiguration": {
                    "CodeCapture": {
                        "CaptureArguments": ["cart"],
                        "CaptureReturn": true,
                        "CaptureLimits": { "MaxHits": 25 },
                    },
                },
                "Tags": [{ "Key": "team", "Value": "checkout" }],
            }),
        },
        Row {
            case: "a Python probe naming the module it instruments",
            stack_name: "appsig-instrumentation-python",
            service: "inventory",
            instrumentation_type: "PROBE",
            properties: json!({
                "InstrumentationType": "PROBE",
                "Service": "inventory",
                "Environment": "ec2:production",
                "SignalType": SNAPSHOT,
                "Location": {
                    "CodeLocation": {
                        "Language": "Python",
                        "CodeUnit": "app.inventory.stock",
                        "MethodName": "reserve",
                        "FilePath": "app/inventory/stock.py",
                    },
                },
                "CaptureConfiguration": minimal_capture(),
            }),
        },
        Row {
            case: "a Javascript breakpoint with only the required location members",
            stack_name: "appsig-instrumentation-javascript",
            service: "web",
            instrumentation_type: "BREAKPOINT",
            properties: json!({
                "InstrumentationType": "BREAKPOINT",
                "Service": "web",
                "Environment": "default",
                "SignalType": SNAPSHOT,
                "Location": {
                    "CodeLocation": {
                        "Language": "Javascript",
                        "FilePath": "src/checkout/cart.js",
                    },
                },
                "CaptureConfiguration": minimal_capture(),
            }),
        },
    ];

    for row in rows {
        let router = router();
        let case = row.case;
        let environment = row.properties["Environment"]
            .as_str()
            .expect("the row declares an environment")
            .to_string();
        let template = json!({
            "Resources": { "Point": { "Type": INSTRUMENTATION_CONFIG, "Properties": row.properties } },
            "Outputs": {
                "PointRef": { "Value": { "Ref": "Point" } },
                "PointArn": { "Value": { "Fn::GetAtt": ["Point", "Arn"] } },
                "CreatedAt": { "Value": { "Fn::GetAtt": ["Point", "CreatedAt"] } },
                "LocationHash": { "Value": { "Fn::GetAtt": ["Point", "LocationHash"] } },
            }
        })
        .to_string();

        let described = deployed(&router, case, row.stack_name, &template).await;
        let published = output(case, &described, "PointRef");
        let hash = output(case, &described, "LocationHash");
        assert_eq!(
            published,
            output(case, &described, "PointArn"),
            "case {case:?}: expected Ref to return the ARN the type publishes, got {published}"
        );
        assert!(
            hash.len() == 16 && hash.chars().all(|held| held.is_ascii_hexdigit()),
            "case {case:?}: expected the 16-character location hash that service computed, got \
             {hash:?}"
        );
        assert!(
            published.ends_with(&format!("/{hash}")),
            "case {case:?}: expected the ARN to carry the location hash, got {published}"
        );
        let created_at = output(case, &described, "CreatedAt");
        assert!(
            !created_at.contains("is not available"),
            "case {case:?}: expected the instant that service recorded, got {created_at}"
        );

        // The point really exists in Application Signals, and the ARN the stack published is
        // the one that service holds for it.
        let (status, read) = get_instrumentation_configuration(
            &router,
            row.service,
            &environment,
            row.instrumentation_type,
            &hash,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Application Signals to report the point the stack created, \
             got {status} with {read}"
        );
        assert_eq!(
            read["Configuration"]["ARN"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the stored record to carry the ARN the stack published, got \
             {read}"
        );
        assert_eq!(
            read["Configuration"]["LocationHash"].as_str(),
            Some(hash.as_str()),
            "case {case:?}: expected the stored record to carry the hash the stack published, \
             got {read}"
        );

        deleted(&router, case, row.stack_name).await;
        let (status, gone) = get_instrumentation_configuration(
            &router,
            row.service,
            &environment,
            row.instrumentation_type,
            &hash,
        )
        .await;
        assert_eq!(
            (status, gone.get("__type").and_then(Value::as_str)),
            (NOT_FOUND_STATUS, Some(RESOURCE_NOT_FOUND)),
            "case {case:?}: expected the point to be gone with the stack, got {status} with \
             {gone}"
        );
    }
}

/// RULE, refusal half: a declaration the OWNING SERVICE refuses fails the RESOURCE — the stack
/// rolls back carrying that service's own sentence on the resource's failed event, inside the
/// envelope every handler refusal is quoted in — and never the template-format refusal a type
/// CloudFormation does not know earns.
///
/// The rows differ along WHICH class of declaration that service refuses: a Python code
/// location that names no module to instrument, and an objective that states both of the two
/// mutually exclusive indicators.
#[tokio::test]
async fn a_declaration_the_owning_service_refuses_fails_the_resource() {
    let rows: Vec<(&str, &str, &str, &str, Value)> = vec![
        (
            "a Python instrumentation point naming no code unit",
            "appsig-refused-code-unit",
            "Point",
            INSTRUMENTATION_CONFIG,
            json!({
                "InstrumentationType": "BREAKPOINT",
                "Service": "inventory",
                "Environment": "ec2:production",
                "SignalType": SNAPSHOT,
                "Location": {
                    "CodeLocation": {
                        "Language": "Python",
                        "MethodName": "reserve",
                        "FilePath": "app/inventory/stock.py",
                    },
                },
                "CaptureConfiguration": minimal_capture(),
            }),
        ),
        (
            "an objective stating both of the mutually exclusive indicators",
            "appsig-refused-two-indicators",
            "Objective",
            SLO,
            json!({
                "Name": "cfn-two-indicators",
                "Sli": {
                    "SliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "checkout",
                            "Environment": "ec2:production",
                        },
                        "MetricType": "LATENCY",
                    },
                    "MetricThreshold": 100.0,
                    "ComparisonOperator": "LessThan",
                },
                "RequestBasedSli": {
                    "RequestBasedSliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "checkout",
                            "Environment": "ec2:production",
                        },
                        "MetricType": "AVAILABILITY",
                    },
                },
            }),
        ),
    ];

    for (case, stack_name, logical_id, resource_type, properties) in rows {
        let router = router();
        let template = json!({
            "Resources": { logical_id: { "Type": resource_type, "Properties": properties } }
        })
        .to_string();

        // The template is ADMITTED — the type is one Amazon Web Services publishes — and it is
        // the RESOURCE that fails.
        submitted(&router, case, stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back, got {reached} with failure reasons \
             {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );

        let reason =
            failed_resource_reason(case, &router, REGION, stack_name, logical_id, "CREATE_FAILED")
                .await;
        assert!(
            !reason.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected the owning service's own refusal, got {reason}"
        );
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_eq!(
            class, "InvalidRequest",
            "case {case:?}: expected the request to be what the provider reported as wrong, got \
             {reason}"
        );
        // The quoted message is that service's own sentence, followed by the diagnostic its SDK
        // appends: the service as its SDK names it, the operation that was refused, the status
        // and the error code.
        let (sentence, diagnostic) = quoted
            .split_once(" (Service: ")
            .unwrap_or_else(|| panic!("case {case:?}: expected the SDK diagnostic, got {quoted}"));
        assert!(
            !sentence.trim().is_empty(),
            "case {case:?}: expected that service's own sentence, got {quoted}"
        );
        assert!(
            diagnostic.starts_with("Application Signals; Operation: "),
            "case {case:?}: expected the SDK's own name for the service and the refused \
             operation, got {quoted}"
        );
        assert!(
            diagnostic.contains("Status Code: 400; Error Code: ValidationException; Request ID: "),
            "case {case:?}: expected the status and the error code that service answered, got \
             {quoted}"
        );
    }
}

/// The Python location rule itself, where the stack meets it: the refusal names the member and
/// the language, and it is Application Signals' own sentence rather than anything
/// CloudFormation worded.
#[tokio::test]
async fn the_refused_python_location_reports_the_services_own_requirement() {
    let router = router();
    let case = "a Python location with no code unit";
    let stack_name = "appsig-python-code-unit";
    let template = json!({
        "Resources": {
            "Point": {
                "Type": INSTRUMENTATION_CONFIG,
                "Properties": {
                    "InstrumentationType": "PROBE",
                    "Service": "pricing",
                    "Environment": "default",
                    "SignalType": SNAPSHOT,
                    "Location": {
                        "CodeLocation": {
                            "Language": "Python",
                            "FilePath": "app/pricing/quote.py",
                        },
                    },
                    "CaptureConfiguration": minimal_capture(),
                }
            }
        }
    })
    .to_string();

    submitted(&router, case, stack_name, &template).await;
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the stack to roll back, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack_name, "Point", "CREATE_FAILED").await;
    let (quoted, _) = quoted_and_class(case, &reason);
    assert!(
        quoted.starts_with("CodeUnit is required for Python locations ("),
        "case {case:?}: expected that service's own requirement, got {quoted}"
    );
}
