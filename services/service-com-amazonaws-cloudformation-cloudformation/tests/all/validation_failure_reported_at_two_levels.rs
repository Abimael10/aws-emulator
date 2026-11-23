// SPDX-License-Identifier: CC0-1.0

//! The TWO LEVELS a failed pre-deployment property validation is reported at, end to end over
//! the awsQuery wire.
//!
//! RULE: a stack operation CloudFormation refuses at a pre-deployment validation reports that
//! refusal on the STACK as the stage's own summary over its errors — how many errors it found
//! and that `DescribeEvents` carries their detail — while each error's own diagnosis is carried
//! ONLY on the validation result recorded for the resource it failed on; no resource of the
//! stack is provisioned, whatever else its template declares. So the stack's own
//! `ResourceStatusReason` never stands in for one error's wording, and a client polling the
//! stack reads that CloudFormation refused the deployment and where to read why.
//!
//! Why it matters: a deployer (a CLI waiter, a CI gate) reports the stack-level reason as THE
//! failure, and keys on that sentence to tell a deployment refused before provisioning, with a
//! countable number of property errors to look up, from a resource that tried and failed
//! mid-deploy. Collapsing the two levels loses the count and the pointer at the detail.
//!
//! The rows differ along what the rule generalises over: WHICH required property the
//! declaration omits (the first its type publishes, a later one), WHICH type and owning service
//! it is of (Amazon SNS's subscription and topic policy, Systems Manager's parameter), and WHAT
//! ELSE the template declares around the failing resource (nothing, a sibling it names, a
//! sibling that reads its attributes) — plus the control row, the same estate with the omitted
//! property supplied, which deploys and is read back in the service that owns it.
//!
//! `required_resource_properties.rs` owns the RESOURCE-level wording of this class and
//! `resource_schema_validation.rs` the two levels for the other property questions; what this
//! suite adds is the STACK level for an omitted required property, across a template whose
//! other declarations would have deployed.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, settled_stack, xml_blocks, xml_text, xml_texts, REGION,
};
use crate::stack_lifecycle::sns;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's contract. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// A router carrying CloudFormation and the two services the estates below really live in, all
/// on ONE engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, which does not mount Amazon SQS: mounting
/// it for every suite of this target would change what the accounts those suites build hold.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// What a stack reports for a failure of a pre-deployment validation: the count of errors the
/// stage found and where their detail is read. The sentence AWS words it in, which is what this
/// suite is about; the crate's own spelling of it is
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the stage words one error of this class: the required property the declaration does not
/// carry. Source: `state::property_validation::missing_property_reason`, whose wording
/// `required_resource_properties.rs` owns.
fn missing_property(property: &str) -> String {
    format!("Required property [{property}] not found")
}

/// What the validation must have recorded about ONE resource: the detail of the error, at the
/// element of the template it was applied to.
struct Detail {
    logical_id: &'static str,
    resource_type: &'static str,
    /// The required property the declaration omits.
    omitted: &'static str,
}

/// One row: the estate a stack is created from, and the error the validation must report — or
/// `None` for the control, whose declarations carry every property their types require.
struct Row {
    case: &'static str,
    stack: String,
    /// The name the row's `AWS::SNS::Topic` declares, so a client can ask Amazon SNS whether
    /// the topic exists at all — `None` for the row whose template declares no topic.
    topic: Option<String>,
    template: String,
    /// Every logical id the template declares, which is what `ListStackResources` reports for a
    /// deployment that ran.
    declared: &'static [&'static str],
    detail: Option<Detail>,
}

/// The queue, the topic and the subscription wiring them together, as a template declares them
/// — the subscription's properties being whatever the row states, so one row can leave one out.
///
/// Every reference between the resources is an intrinsic (`Ref`, `Fn::GetAtt`), so the failing
/// declaration sits beside siblings it names and siblings whose attributes it reads.
fn fan_out_template(topic: &str, queue: &str, subscription: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Queue": {"Type": "AWS::SQS::Queue", "Properties": {"QueueName": queue}},
            "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": topic}},
            "Subscription": {
                "Type": "AWS::SNS::Subscription",
                "Properties": subscription,
            },
        },
    })
    .to_string()
}

/// The rows, in the order the test drives them.
fn rows() -> Vec<Row> {
    let names = |index: usize| {
        (
            format!("two-level-validation-{index}"),
            format!("two-level-validation-topic-{index}"),
            format!("two-level-validation-queue-{index}"),
        )
    };
    let (stack, topic, queue) = names(1);
    let mut rows = vec![Row {
        case: "a subscription that omits the protocol it is delivered over, beside the topic \
               it is on and the queue it reads the endpoint off",
        stack,
        topic: Some(topic.clone()),
        template: fan_out_template(
            &topic,
            &queue,
            json!({
                "TopicArn": {"Ref": "Topic"},
                "Endpoint": {"Fn::GetAtt": ["Queue", "Arn"]},
            }),
        ),
        declared: &["Queue", "Subscription", "Topic"],
        detail: Some(Detail {
            logical_id: "Subscription",
            resource_type: "AWS::SNS::Subscription",
            omitted: "Protocol",
        }),
    }];

    // The same type, missing the OTHER property its contract requires — the later one in the
    // order the contract publishes them — in a template that declares nothing else.
    let (stack, _, _) = names(2);
    rows.push(Row {
        case: "a subscription that omits the topic it is on, alone in its template",
        stack,
        topic: None,
        template: json!({
            "Resources": {
                "Subscription": {
                    "Type": "AWS::SNS::Subscription",
                    "Properties": {"Protocol": "email", "Endpoint": "ops@example.invalid"},
                },
            },
        })
        .to_string(),
        declared: &["Subscription"],
        detail: Some(Detail {
            logical_id: "Subscription",
            resource_type: "AWS::SNS::Subscription",
            omitted: "TopicArn",
        }),
    });

    // Another type of the same service, whose declaration omits the resources the policy is
    // installed on while naming the topic the same template declares.
    let (stack, topic, _) = names(3);
    rows.push(Row {
        case: "a topic policy that omits the topics it is installed on, beside that topic",
        stack,
        topic: Some(topic.clone()),
        template: json!({
            "Resources": {
                "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": topic}},
                "Access": {
                    "Type": "AWS::SNS::TopicPolicy",
                    "Properties": {
                        "PolicyDocument": {"Version": "2012-10-17", "Statement": []},
                    },
                },
            },
        })
        .to_string(),
        declared: &["Access", "Topic"],
        detail: Some(Detail {
            logical_id: "Access",
            resource_type: "AWS::SNS::TopicPolicy",
            omitted: "Topics",
        }),
    });

    // A type of ANOTHER service, to pin that the stack's sentence is the stage's rather than
    // the owning service's: the parameter omits the value it is, beside a topic that would have
    // deployed.
    let (stack, topic, _) = names(4);
    rows.push(Row {
        case: "a parameter that omits the value it carries, beside a topic that would deploy",
        stack,
        topic: Some(topic.clone()),
        template: json!({
            "Resources": {
                "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": topic}},
                "Setting": {
                    "Type": "AWS::SSM::Parameter",
                    "Properties": {"Type": "String"},
                },
            },
        })
        .to_string(),
        declared: &["Setting", "Topic"],
        detail: Some(Detail {
            logical_id: "Setting",
            resource_type: "AWS::SSM::Parameter",
            omitted: "Value",
        }),
    });

    // The control: the first row's estate with the omitted property supplied, which deploys.
    let (stack, topic, queue) = names(5);
    rows.push(Row {
        case: "the same estate with the protocol supplied",
        stack,
        topic: Some(topic.clone()),
        template: fan_out_template(
            &topic,
            &queue,
            json!({
                "TopicArn": {"Ref": "Topic"},
                "Protocol": "sqs",
                "Endpoint": {"Fn::GetAtt": ["Queue", "Arn"]},
            }),
        ),
        declared: &["Queue", "Subscription", "Topic"],
        detail: None,
    });
    rows
}

/// Every validation result `DescribeEvents` reports for one operation, as the members a
/// validation result carries.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<Vec<String>> {
    let (code, events) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {events}"
    );
    xml_blocks(&events, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            [
                "LogicalResourceId",
                "ResourceType",
                "EventType",
                "ValidationName",
                "ValidationStatus",
                "ValidationFailureMode",
                "ValidationStatusReason",
                "ValidationPath",
            ]
            .iter()
            .map(|field| xml_text(event, field).unwrap_or_default())
            .collect()
        })
        .collect()
}

/// Every row: the stack's own failure event carries the stage's summary and nothing of one
/// error's wording, that wording is carried on the resource's validation result, and nothing
/// the template declared was provisioned — while the control's estate is deployed and read back
/// in the service that owns it.
#[tokio::test]
async fn a_refused_deployment_summarises_on_the_stack_and_details_on_the_resource() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own operation, not what
        // another row left behind.
        let router = router();
        let stack = row.stack.as_str();

        let (code, created) = create_stack(&router, stack, &row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );
        let described = settled_stack(case, &router, REGION, stack).await;
        let expected_status = match row.detail {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // The DETAIL: an event about the resource NOTHING was done to, naming the validation,
        // its status, how its failure was handled, the property the declaration does not carry
        // and the element of the template the validation was applied to.
        let results = validation_results(&router, case, stack).await;
        let expected_results: Vec<Vec<String>> = row
            .detail
            .as_ref()
            .map(|detail| {
                vec![vec![
                    detail.logical_id.to_string(),
                    detail.resource_type.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    "FAILED".to_string(),
                    "FAIL".to_string(),
                    missing_property(detail.omitted),
                    format!("/Resources/{}/Properties", detail.logical_id),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // ...and the SUMMARY, on the stack's own failure event: the count of errors the stage
        // found, the pointer at the detail above, and the detailed status the model gives a
        // validation failure.
        let (code, stack_events) =
            call(&router, "DescribeStackEvents", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the stack's events must be readable, got {code} {stack_events}"
        );
        let own_events: Vec<String> = xml_blocks(&stack_events, "member")
            .iter()
            .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(stack))
            .map(|event| {
                format!(
                    "{}|{}|{}",
                    xml_text(event, "ResourceStatus").unwrap_or_default(),
                    xml_text(event, "DetailedStatus").unwrap_or_default(),
                    xml_text(event, "ResourceStatusReason").unwrap_or_default(),
                )
            })
            .collect();
        let expected_failure: Vec<String> = row
            .detail
            .as_ref()
            .map(|_| {
                vec![format!(
                    "{}|{VALIDATION_FAILED}|{}",
                    resource_status::CREATE_FAILED,
                    summary(1)
                )]
            })
            .unwrap_or_default();
        let failures: Vec<String> = own_events
            .iter()
            .filter(|event| event.starts_with(&format!("{}|", resource_status::CREATE_FAILED)))
            .cloned()
            .collect();
        assert_eq!(
            failures, expected_failure,
            "case {case:?}: expected the stack's own failure event(s) {expected_failure:?}, got \
             {own_events:?}"
        );

        // The two levels say different things ON PURPOSE: no event of the STACK repeats the
        // error's own wording, which is what a client reading the stack's reason would
        // otherwise be handed instead of the count and the pointer.
        if let Some(detail) = &row.detail {
            let wording = missing_property(detail.omitted);
            assert!(
                !own_events.iter().any(|event| event.contains(&wording)),
                "case {case:?}: expected no event of the stack to carry the error's own \
                 wording {wording:?}, got {own_events:?}"
            );
        }

        // Nothing the refused template declared was provisioned: the stack manages no resource
        // at all, while the control's deployment manages every one it declared.
        let (code, listed) = call(&router, "ListStackResources", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: ListStackResources must answer 200, got {code} {listed}"
        );
        let mut managed = xml_texts(&listed, "LogicalResourceId");
        managed.sort();
        let expected_managed: Vec<String> = match row.detail {
            Some(_) => Vec::new(),
            None => row.declared.iter().map(|id| id.to_string()).collect(),
        };
        assert_eq!(
            managed, expected_managed,
            "case {case:?}: expected ListStackResources to report {expected_managed:?}, got \
             {listed}"
        );

        // ...and the service that OWNS the resource is the proof: the refused deployments left
        // Amazon SNS no topic under the name their templates declared, and the control's topic
        // is really there.
        if let Some(declared_topic) = &row.topic {
            let (code, topics) = sns(&router, "ListTopics", &[]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: ListTopics must answer 200, got {code} {topics}"
            );
            assert_eq!(
                topics.contains(declared_topic.as_str()),
                row.detail.is_none(),
                "case {case:?}: expected Amazon SNS to hold the topic {declared_topic:?} only \
                 where the deployment ran, got {topics}"
            );
        }
    }
}
