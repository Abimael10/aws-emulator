// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetResourceMetricsConfiguration`: the configuration this service holds
//! for one resource, the resources that can hold one at all, and the two refusals a read of one
//! makes.
//!
//! Every table row is a distinct input along what the rule under test generalises over (the
//! service and resource type the ARN names, the partition and Region it names them in, whether
//! the resource part carries a type and an id at all, how many metric names a selection lists,
//! whether a selection is present, and the Region the request is signed for), so neither the
//! resource type a configuration is held for nor the one that is refused is a single case the
//! suite was written around.
//!
//! The configurations are seeded through the store those configurations' writers write
//! (`state::ComAmazonawsCloudwatchGraniteserviceversion20100801State::keep_resource_metrics_configuration`),
//! because `CreateResourceMetricsConfiguration` and `UpdateResourceMetricsConfiguration` are
//! still scaffolds — each the subject of its own stub issue — so what this suite reads back is
//! the store a read reports from rather than anything the read itself made up.

use crate::harness::{
    build_router_with_engine, call, call_in_query_mode, call_in_region, error_code, error_message,
};
use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    ResourceMetricSelection, ResourceMetricsConfiguration,
};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use smithy_server_runtime::MultiServiceRouter;

/// The shape id the body names the request-validation class under, which is the class this
/// service refuses a request it cannot serve in: its legacy code (`ValidationError`, the code a
/// CloudWatch client branches on) travels in the `x-amzn-query-error` header instead.
const VALIDATION_SHAPE_ID: &str = "com.amazon.coral.validate#ValidationException";

/// The header a query-compatible client reads that legacy code and the fault side from.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The legacy report of the request-validation class.
const VALIDATION_REPORT: &str = "ValidationError;Sender";

/// What a read refuses a resource this service collects no detailed metrics for with.
const UNSUPPORTED_RESOURCE_TYPE: &str = "Unsupported resource type";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// An instant to hold a seeded configuration's `CreatedAt` at: epoch seconds, so the suite
/// asserts on the instant the store holds rather than on when it ran.
const CREATED_SECS: f64 = 1_700_000_000.0;

/// The instant a seeded configuration was last updated at, later than [`CREATED_SECS`] so a row
/// that reports both cannot pass by reporting one of them twice.
const UPDATED_SECS: f64 = 1_700_086_400.5;

/// The ARN of one EC2 instance, in `partition` and `region`.
fn instance_arn(partition: &str, region: &str, instance_id: &str) -> String {
    format!("arn:{partition}:ec2:{region}:{ACCOUNT}:instance/{instance_id}")
}

/// The ARN of one EBS volume, in `region`.
fn volume_arn(region: &str, volume_id: &str) -> String {
    format!("arn:aws:ec2:{region}:{ACCOUNT}:volume/{volume_id}")
}

/// Hold a configuration for `resource_arn` in the store, carrying `include_metrics` as its one
/// metric selection when the case states one, and answer it.
fn given_configuration(
    engine: &engine::InMemoryEngine,
    resource_arn: &str,
    include_metrics: Option<Vec<String>>,
) -> ResourceMetricsConfiguration {
    let configuration = ResourceMetricsConfiguration {
        resource_arn: resource_arn.to_string(),
        created_at: cloudwatch::to_timestamp(CREATED_SECS),
        updated_at: cloudwatch::to_timestamp(UPDATED_SECS),
        metric_selections: include_metrics
            .map(|include_metrics| vec![ResourceMetricSelection { include_metrics }]),
    };
    let state = cloudwatch::service_state(engine).expect("the service store is reachable");
    // The guard is a named local, so it is dropped before the configuration it answers with is
    // used.
    let mut store = state.lock();
    store.keep_resource_metrics_configuration(configuration.clone());
    configuration
}

/// How many configurations the store holds, which is what a read must leave exactly as it was.
fn held_configurations(engine: &engine::InMemoryEngine) -> Vec<String> {
    let state = cloudwatch::service_state(engine).expect("the service store is reachable");
    let store = state.lock();
    let mut arns: Vec<String> = store.resource_metrics_configurations.keys().cloned().collect();
    arns.sort();
    arns
}

/// Read the configuration of `resource_arn`, answering the HTTP status and body.
async fn read(router: &MultiServiceRouter, resource_arn: &str) -> (u16, Value) {
    call(
        router,
        "GetResourceMetricsConfiguration",
        &json!({"ResourceArn": resource_arn}),
    )
    .await
}

/// The configuration a successful read reports.
fn reported(body: &Value) -> &Value {
    body.get("ResourceMetricsConfiguration")
        .expect("a successful read reports the configuration")
}

/// A read reports the configuration the store holds, whichever resource whose detailed metrics
/// this service collects it is held for, and however the configuration selects metrics.
#[tokio::test]
async fn reports_the_configuration_the_store_holds() {
    // The rows span the resource type (an instance and a volume), the partition and Region the
    // ARN names, and the metric selection (absent, one name, several names, and the modelled
    // maximum of `MetricNameList` — `@length 1..500`,
    // `smithy_query(query="shape",shape="MetricNameList")`).
    let many: Vec<String> = (0..500).map(|index| format!("Metric{index}")).collect();
    let cases: [(&str, String, Option<Vec<String>>); 5] = [
        (
            "an instance, collecting every available metric",
            instance_arn("aws", "us-east-1", "i-0123456789abcdef0"),
            None,
        ),
        (
            "an instance, one metric named",
            instance_arn("aws", "eu-west-1", "i-00000000000000001"),
            Some(vec!["CPUUtilization".to_string()]),
        ),
        (
            "an instance in another partition",
            instance_arn("aws-us-gov", "us-gov-west-1", "i-abcdefabcdefabcde"),
            Some(vec!["NetworkIn".to_string(), "NetworkOut".to_string()]),
        ),
        (
            "a volume",
            volume_arn("ap-southeast-2", "vol-0fedcba9876543210"),
            Some(vec![
                "VolumeReadOps".to_string(),
                "VolumeWriteOps".to_string(),
                "VolumeQueueLength".to_string(),
            ]),
        ),
        (
            "a volume, as many metrics named as one selection holds",
            volume_arn("sa-east-1", "vol-00000000000000002"),
            Some(many),
        ),
    ];

    for (case, resource_arn, include_metrics) in cases {
        let (router, engine) = build_router_with_engine();
        let held = given_configuration(&engine, &resource_arn, include_metrics.clone());
        let (status, body) = read(&router, &resource_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the configuration must answer HTTP 200, got {status} with \
             body {body}"
        );
        let configuration = reported(&body);
        assert_eq!(
            configuration.get("ResourceArn").and_then(Value::as_str),
            Some(resource_arn.as_str()),
            "case {case:?}: the configuration must be reported for the resource it is held for, \
             got {configuration}"
        );
        assert_eq!(
            configuration.get("CreatedAt").and_then(Value::as_f64),
            Some(cloudwatch::from_timestamp(&held.created_at)),
            "case {case:?}: the creation instant must be the one the store holds, got \
             {configuration}"
        );
        assert_eq!(
            configuration.get("UpdatedAt").and_then(Value::as_f64),
            Some(cloudwatch::from_timestamp(&held.updated_at)),
            "case {case:?}: the update instant must be the one the store holds, got \
             {configuration}"
        );
        let selections: Option<Vec<Vec<String>>> =
            configuration.get("MetricSelections").map(|reported| {
                reported
                    .as_array()
                    .expect("the metric selections are reported as an array")
                    .iter()
                    .map(|selection| {
                        selection
                            .get("IncludeMetrics")
                            .and_then(Value::as_array)
                            .expect("a selection names the metrics it includes")
                            .iter()
                            .map(|name| {
                                name.as_str()
                                    .expect("a metric name is reported as text")
                                    .to_string()
                            })
                            .collect()
                    })
                    .collect()
            });
        let expected: Option<Vec<Vec<String>>> = include_metrics.map(|names| vec![names]);
        assert_eq!(
            selections, expected,
            "case {case:?}: the metric selections must be the ones the store holds, got \
             {configuration}"
        );
    }
}

/// A read of a resource whose detailed metrics this service collects none of is refused as a
/// fault of the request, whatever that resource is — including a resource part that names no
/// type of resource at all.
#[tokio::test]
async fn refuses_a_resource_no_detailed_metrics_are_collected_for() {
    // The rows span the service namespace, the resource type within a service that DOES carry
    // detailed metrics for other types, the separator the resource part uses, and a resource
    // part carrying no type or no id at all. Each ARN satisfies the modelled `ResourceArn`
    // pattern (`^arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*:\d{12}:.+$`,
    // `smithy_query(query="shape",shape="ResourceArn")`), so each one reaches the handler.
    let cases: [(&str, String); 9] = [
        (
            "a queue",
            format!("arn:aws:sqs:us-east-1:{ACCOUNT}:orders"),
        ),
        (
            "a function",
            format!("arn:aws:lambda:us-east-1:{ACCOUNT}:function:checkout"),
        ),
        (
            "a table",
            format!("arn:aws:dynamodb:us-east-1:{ACCOUNT}:table/orders"),
        ),
        (
            "a role, whose ARN names no Region",
            format!("arn:aws:iam::{ACCOUNT}:role/service-role"),
        ),
        (
            "an alarm of this service",
            format!("arn:aws:cloudwatch:us-east-1:{ACCOUNT}:alarm:cpu-high"),
        ),
        (
            "another type of the same service as a supported one",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT}:security-group/sg-0123456789abcdef0"),
        ),
        (
            "a network of the same service as a supported one",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT}:vpc/vpc-0123456789abcdef0"),
        ),
        (
            "a resource part naming a type with no id",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT}:instance"),
        ),
        (
            "a resource part naming a type with an empty id",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT}:instance/"),
        ),
    ];

    let (router, _engine) = build_router_with_engine();
    for (case, resource_arn) in cases {
        let (status, headers, body) = call_in_query_mode(
            &router,
            "GetResourceMetricsConfiguration",
            &json!({"ResourceArn": resource_arn}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a resource no detailed metrics are collected for must be refused \
             with HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            body.get("__type").and_then(Value::as_str),
            Some(VALIDATION_SHAPE_ID),
            "case {case:?}: the refusal must carry the request-validation class, got {body}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, VALIDATION_REPORT,
            "case {case:?}: the refusal must report the code a CloudWatch client branches on, \
             got {reported:?} with body {body}"
        );
        assert_eq!(
            error_message(&body),
            UNSUPPORTED_RESOURCE_TYPE,
            "case {case:?}: the refusal must say what about the request cannot be served, got \
             {body}"
        );
    }
}

/// The ARN is judged BEFORE anything is looked up: a resource no detailed metrics are collected
/// for is refused even when the store happens to hold a configuration under that very ARN, which
/// no writer of a configuration could have left there.
#[tokio::test]
async fn judges_the_resource_before_looking_a_configuration_up() {
    let (router, engine) = build_router_with_engine();
    let resource_arn = format!("arn:aws:sqs:us-east-1:{ACCOUNT}:orders");
    given_configuration(&engine, &resource_arn, None);

    let (status, body) = read(&router, &resource_arn).await;
    assert_eq!(
        status, 400,
        "case \"a held configuration of an unsupported resource\": the request must still be \
         refused with HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_message(&body),
        UNSUPPORTED_RESOURCE_TYPE,
        "case \"a held configuration of an unsupported resource\": the refusal must be the one \
         the resource earns, got {body}"
    );
}

/// A read of a resource that CAN hold a configuration, but holds none, reports that the
/// configuration does not exist — naming the kind of resource that was looked up and the ARN it
/// was looked up by.
#[tokio::test]
async fn reports_a_configuration_that_does_not_exist() {
    let seeded = instance_arn("aws", "us-east-1", "i-0123456789abcdef0");
    // The rows span the resource type, the partition and Region, and whether the store holds a
    // configuration for ANOTHER resource of the same type.
    let cases: [(&str, String, bool); 4] = [
        (
            "an instance nothing is held for, with an empty store",
            instance_arn("aws", "us-east-1", "i-00000000000000003"),
            false,
        ),
        (
            "an instance nothing is held for, beside one that is",
            instance_arn("aws", "us-east-1", "i-00000000000000004"),
            true,
        ),
        (
            "a volume nothing is held for",
            volume_arn("eu-central-1", "vol-00000000000000005"),
            true,
        ),
        (
            "an instance of another partition nothing is held for",
            instance_arn("aws-cn", "cn-north-1", "i-00000000000000006"),
            false,
        ),
    ];

    for (case, resource_arn, hold_another) in cases {
        let (router, engine) = build_router_with_engine();
        if hold_another {
            given_configuration(&engine, &seeded, None);
        }
        let (status, body) = read(&router, &resource_arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a configuration that does not exist must be reported with HTTP 404, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: the refusal must carry the modelled code, got {body}"
        );
        assert_eq!(
            body.get("ResourceType").and_then(Value::as_str),
            Some("ResourceMetricsConfiguration"),
            "case {case:?}: the refusal must name the kind of resource looked up, got {body}"
        );
        assert_eq!(
            body.get("ResourceId").and_then(Value::as_str),
            Some(resource_arn.as_str()),
            "case {case:?}: the refusal must name the resource it was looked up by, got {body}"
        );
        assert_eq!(
            held_configurations(&engine),
            if hold_another {
                vec![seeded.clone()]
            } else {
                Vec::new()
            },
            "case {case:?}: a read that found nothing must not create anything"
        );
    }
}

/// Reading a configuration changes nothing a later caller sees, and the Region the request is
/// signed for does not select which configuration is reported: a configuration is held under the
/// ARN of the resource it applies to, and that ARN names the Region that resource lives in.
#[tokio::test]
async fn reading_changes_nothing_a_later_caller_sees() {
    let (router, engine) = build_router_with_engine();
    let resource_arn = instance_arn("aws", "us-east-1", "i-0123456789abcdef0");
    given_configuration(&engine, &resource_arn, Some(vec!["CPUUtilization".to_string()]));

    let (first_status, first) = read(&router, &resource_arn).await;
    assert_eq!(
        first_status, 200,
        "case \"the first read\": reading must answer HTTP 200, got {first_status} with body \
         {first}"
    );

    // The same resource, read through another Region's endpoint: the ARN names the resource, so
    // the credential scope of the request cannot change what is reported.
    for region in ["us-east-1", "eu-west-1", "ap-northeast-1"] {
        let (status, again) = call_in_region(
            &router,
            region,
            "GetResourceMetricsConfiguration",
            &json!({"ResourceArn": resource_arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region:?}: reading must answer HTTP 200, got {status} with body {again}"
        );
        assert_eq!(
            reported(&again),
            reported(&first),
            "case {region:?}: every read must report the configuration the first one did"
        );
    }

    assert_eq!(
        held_configurations(&engine),
        vec![resource_arn],
        "case \"after the reads\": the store must hold exactly what it held before them"
    );
}
