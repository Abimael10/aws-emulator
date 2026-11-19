// SPDX-License-Identifier: CC0-1.0

//! The utilization a capacity provider's MANAGED SCALING may be asked to hold its Auto
//! Scaling group at.
//!
//! RULE under test: `managedScaling.targetCapacity` is a percentage — "the target capacity
//! utilization as a percentage for the capacity provider. The specified value must be greater
//! than `0` and less than or equal to `100`" (`smithy_query(query="shape",
//! shape="ManagedScaling")`, whose `targetCapacity` targets `ManagedScalingTargetCapacity`
//! with `@range` min 1 max 100). A `CreateCapacityProvider` or `UpdateCapacityProvider`
//! naming a value outside that range is refused with HTTP 400, the error code
//! `ClientException` and the fixed sentence `The managed scaling target capacity value is
//! invalid. Specify an integer greater than 0 and less than or equal to 100 and try again.`,
//! and nothing is written: the provider the create named does not exist, and the one an
//! update named still reports the utilization it held. A value inside the range is stored and
//! reported back, and a request that names none at all leaves the provider at the default
//! that member documents ("the default value of `100` percent results in the Amazon EC2
//! instances in your Auto Scaling group being completely used").
//!
//! The error CLASS is the point: both `ClientException` and `InvalidParameterException` are
//! 400s this operation models, but they are distinct SDK exception types, so a client that
//! branches on the code to tell "this value is one I will not take" from "this request is
//! structurally wrong" has to see the one AWS answers.
//!
//! Why the suite lives beside the router: `autoScalingGroupProvider` binds a provider onto an
//! Auto Scaling group of the caller's, and Amazon ECS resolves that group against the Amazon
//! EC2 Auto Scaling service's own records — so every group below is created through THAT
//! service's API and its ARN is read back from it, and the two services have to agree for a
//! row to get as far as the managed-scaling block at all.
//!
//! The rows differ along what the rule generalises over: where the value sits relative to the
//! published bounds (each bound itself, one step outside either end, a value well past the
//! top, a negative one, one well inside) and which operation carries it (the create and the
//! update).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The error class and the whole sentence a refused managed-scaling target carries. Spelled
/// out here rather than imported, so a wrong wording in the implementation cannot make a row
/// pass.
const CLIENT_EXCEPTION: &str = "ClientException";
const TARGET_CAPACITY_INVALID: &str = "The managed scaling target capacity value is invalid. \
                                       Specify an integer greater than 0 and less than or \
                                       equal to 100 and try again.";

/// The bounds the member publishes, and the default it documents.
const LOWEST_TARGET: i64 = 1;
const HIGHEST_TARGET: i64 = 100;
const DEFAULT_TARGET: i64 = 100;

/// One create row: the target the request names (or none at all), and what the account holds
/// afterwards.
struct CreateRow {
    case: &'static str,
    provider: &'static str,
    group: &'static str,
    /// `managedScaling.targetCapacity` as the request carries it, or `None` for a request
    /// that names no `managedScaling` block at all.
    target: Option<i64>,
    /// The utilization the stored provider must report, or `None` for a row that must create
    /// nothing.
    stored: Option<i64>,
}

const CREATE_ROWS: &[CreateRow] = &[
    CreateRow {
        case: "the lowest utilization the member publishes",
        provider: "tclowest",
        group: "tc-asg-lowest",
        target: Some(LOWEST_TARGET),
        stored: Some(LOWEST_TARGET),
    },
    CreateRow {
        case: "the highest utilization the member publishes",
        provider: "tchighest",
        group: "tc-asg-highest",
        target: Some(HIGHEST_TARGET),
        stored: Some(HIGHEST_TARGET),
    },
    CreateRow {
        case: "a utilization well inside the range",
        provider: "tcinside",
        group: "tc-asg-inside",
        target: Some(40),
        stored: Some(40),
    },
    CreateRow {
        case: "no managed scaling block at all",
        provider: "tcdefaulted",
        group: "tc-asg-defaulted",
        target: None,
        stored: Some(DEFAULT_TARGET),
    },
    CreateRow {
        case: "one step below the lowest utilization",
        provider: "tcbelow",
        group: "tc-asg-below",
        target: Some(LOWEST_TARGET - 1),
        stored: None,
    },
    CreateRow {
        case: "one step past the highest utilization",
        provider: "tcpast",
        group: "tc-asg-past",
        target: Some(HIGHEST_TARGET + 1),
        stored: None,
    },
    CreateRow {
        case: "a utilization well past the highest",
        provider: "tcfarpast",
        group: "tc-asg-farpast",
        target: Some(150),
        stored: None,
    },
    CreateRow {
        case: "a negative utilization",
        provider: "tcnegative",
        group: "tc-asg-negative",
        target: Some(-5),
        stored: None,
    },
];

/// One emulator carrying Amazon ECS, Amazon EC2 Auto Scaling — which owns the groups a
/// provider binds onto — and Amazon EC2, which owns the launch template a group launches
/// from, over ONE shared engine, as the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_autoscaling_autoscaling_2011_01_01::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// The Amazon EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The launch template every group below launches from, and the machine it describes.
const TEMPLATE: &str = "tc-capacity-template";
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// One `ec2Query` Amazon EC2 call that has to succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected ec2 {action} to answer 200, got {status} with {payload}"
    );
    payload
}

/// Create the launch template every group of this suite launches from, through Amazon EC2's
/// own API: a group names a template that exists, and that service owns it.
async fn launch_template(router: &MultiServiceRouter, case: &str) {
    ec2(
        router,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; no signature is checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
    )
}

/// One `awsJson1_1` Amazon ECS call, answered with the HTTP status and the parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that has to succeed.
async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {answer}"
    );
    answer
}

/// The Amazon EC2 Auto Scaling API version an `awsQuery` body carries.
const AUTO_SCALING_API_VERSION: &str = "2011-01-01";

/// One `awsQuery` Amazon EC2 Auto Scaling call that has to succeed, answered with its XML
/// body.
async fn auto_scaling(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={AUTO_SCALING_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://autoscaling.{REGION}.amazonaws.com/"))
        .header("host", format!("autoscaling.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("autoscaling"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected autoscaling {action} to answer 200, got {status} with \
         {payload}"
    );
    payload
}

/// Create one Auto Scaling group through that service's own API and answer the ARN it minted
/// for it, read back through its own `DescribeAutoScalingGroups`.
///
/// The smallest group that API accepts: a name, a zone of the Region to place instances in, a
/// launch template to launch them from, and a size range that holds none — so nothing is
/// launched and no transition is owed.
async fn group_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let zone = format!("{REGION}a");
    auto_scaling(
        router,
        case,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "0"),
            ("AvailabilityZones.member.1", zone.as_str()),
        ],
    )
    .await;
    let described = auto_scaling(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    element(&described, "AutoScalingGroupARN")
}

/// The error code a refusal reports, as the short shape name its `__type` carries.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `autoScalingGroupProvider` a request carries for `group`, with `target` named in its
/// managed-scaling block when the row names one.
fn provider_configuration(group: &str, target: Option<i64>) -> Value {
    let mut configuration = json!({ "autoScalingGroupArn": group });
    if let Some(target) = target {
        configuration["managedScaling"] = json!({
            "status": "ENABLED",
            "targetCapacity": target,
        });
    }
    configuration
}

/// The capacity provider `name` as `DescribeCapacityProviders` reports it, and `Value::Null`
/// when the account holds none by that name.
async fn read_provider(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let body = ecs_ok(
        router,
        case,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": [name] }),
    )
    .await;
    body["capacityProviders"]
        .as_array()
        .and_then(|providers| providers.first().cloned())
        .unwrap_or(Value::Null)
}

/// The utilization a reported provider's managed scaling targets.
fn targeted(provider: &Value) -> Option<i64> {
    provider["autoScalingGroupProvider"]["managedScaling"]["targetCapacity"].as_i64()
}

/// Assert that `answer` is the refusal a target outside the range earns.
fn assert_refused(case: &str, status: u16, answer: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected a utilization outside the published range to be refused \
         with 400, got {status} with {answer}"
    );
    let code = error_code(answer);
    assert_eq!(
        code, CLIENT_EXCEPTION,
        "case {case:?}: expected the refusal to carry {CLIENT_EXCEPTION}, got {code:?} in \
         {answer}"
    );
    assert_eq!(
        answer["message"].as_str(),
        Some(TARGET_CAPACITY_INVALID),
        "case {case:?}: expected the refusal's whole message to be \
         {TARGET_CAPACITY_INVALID:?}, got {answer}"
    );
}

#[tokio::test]
async fn a_create_takes_only_a_target_utilization_inside_the_published_range() {
    let router = emulator();
    launch_template(&router, "setup").await;
    for row in CREATE_ROWS {
        let case = row.case;
        let group = group_arn(&router, case, row.group).await;
        let (status, answer) = ecs(
            &router,
            "CreateCapacityProvider",
            json!({
                "name": row.provider,
                "autoScalingGroupProvider": provider_configuration(&group, row.target),
            }),
        )
        .await;

        match row.stored {
            None => {
                assert_refused(case, status, &answer);
                // Nothing was written: the name the refused create carried names no provider
                // of the account.
                let read = read_provider(&router, case, row.provider).await;
                assert_eq!(
                    read,
                    Value::Null,
                    "case {case:?}: expected the refused create to leave no provider named \
                     {:?}, got {read}",
                    row.provider
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to be served with 200, got {status} \
                     with {answer}"
                );
                let read = read_provider(&router, case, row.provider).await;
                assert_eq!(
                    targeted(&read),
                    Some(expected),
                    "case {case:?}: expected the stored provider to target {expected}, got \
                     {read}"
                );
            }
        }
    }
}

/// The utilization the provider the update rows act on is created with, so a refused update
/// is read against a value a request put there rather than against the default.
const HELD_TARGET: i64 = 75;

#[tokio::test]
async fn an_update_takes_only_a_target_utilization_inside_the_published_range() {
    let router = emulator();
    let case = "an update of a provider's target utilization";
    launch_template(&router, case).await;
    let provider = "tcupdated";
    let group = group_arn(&router, case, "tc-asg-updated").await;
    ecs_ok(
        &router,
        case,
        "CreateCapacityProvider",
        json!({
            "name": provider,
            "autoScalingGroupProvider": provider_configuration(&group, Some(HELD_TARGET)),
        }),
    )
    .await;

    // (the case, the target the update names, the utilization the provider holds after it)
    let rows: [(&str, i64, i64); 6] = [
        ("one step past the highest utilization", HIGHEST_TARGET + 1, HELD_TARGET),
        ("one step below the lowest utilization", LOWEST_TARGET - 1, HELD_TARGET),
        ("a utilization well past the highest", 150, HELD_TARGET),
        ("a negative utilization", -5, HELD_TARGET),
        ("the lowest utilization the member publishes", LOWEST_TARGET, LOWEST_TARGET),
        ("the highest utilization the member publishes", HIGHEST_TARGET, HIGHEST_TARGET),
    ];
    for (case, target, held) in rows {
        let (status, answer) = ecs(
            &router,
            "UpdateCapacityProvider",
            json!({
                "name": provider,
                "autoScalingGroupProvider": {
                    "managedScaling": { "status": "ENABLED", "targetCapacity": target },
                },
            }),
        )
        .await;
        if target == held {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served with 200, got {status} with \
                 {answer}"
            );
        } else {
            assert_refused(case, status, &answer);
        }
        let read = read_provider(&router, case, provider).await;
        assert_eq!(
            targeted(&read),
            Some(held),
            "case {case:?}: expected the provider to target {held} after the update, got \
             {read}"
        );
    }
}
