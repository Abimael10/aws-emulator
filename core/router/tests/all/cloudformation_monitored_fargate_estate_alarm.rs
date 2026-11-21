// SPDX-License-Identifier: CC0-1.0

//! A monitored Fargate estate deployed as ONE stack: the alarm it declares beside the workload
//! is the alarm Amazon CloudWatch holds.
//!
//! The rule under test: **every resource a stack declares is created in the service that owns it
//! before the stack reports CREATE_COMPLETE, with its declared properties read as the TYPES that
//! service's own model states for the members they reach** — a template's scalar is text
//! wherever its author quoted it and wherever a `Ref` to a parameter resolved it, while a
//! cadence, a count and a threshold are modelled as numbers, so an alarm whose period comes from
//! a parameter is really put rather than refused by the owning deserializer. A stack whose alarm
//! resource failed that way reports the whole deployment rolled back, and a client that deployed
//! its monitoring as infrastructure has neither the alarm nor the estate it watches.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the put, Amazon CloudWatch
//! serves and answers for the alarm afterwards, and Amazon ECS, Amazon EC2, CloudWatch Logs,
//! Systems Manager, X-Ray and IAM own the rest of the estate the same template declares — so the
//! suite lives here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix Amazon CloudWatch's model fixes, whose `sigv4Name` is `monitoring`.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// How a stack read waits for the clock-owed spans of a deployment to come due: look again,
/// with a deadline generous enough that a loaded host still reaches it. What is waited FOR is
/// the stack's terminal status appearing, which this emulator makes due at an instant, so the
/// bound only decides when a failure is reported as one.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 1_200;

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
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

/// Call one Amazon CloudWatch operation over its awsJson1_0 wire.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Read a stack until it reports a status that is no longer in progress, and answer that read's
/// body. The deadline is only here so a failure is a failure rather than a hang.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {stack} never left an in-progress status, last read {described}")
}

/// The reason each FAILED resource of the stack carries, as `(logical id, reason)` — read from
/// the event member that carries both, so no event's reason is paired with another's id.
async fn failures_of(router: &MultiServiceRouter, stack: &str) -> Vec<(String, String)> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.ends_with("_FAILED") {
                return None;
            }
            let id = elements(member, "LogicalResourceId").into_iter().next()?;
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some((id, reason))
        })
        .collect()
}

/// The template this suite deploys: the network a Fargate service runs in, the service itself,
/// the log group its containers write to, the parameter and the X-Ray group the workload reads,
/// and the latency alarm over the metric the workload publishes.
const TEMPLATE: &str = r#"{
  "Parameters": {
    "Namespace": { "Type": "String", "Default": "Flint/Frames" },
    "AlarmPeriod": { "Type": "Number", "Default": 30 }
  },
  "Resources": {
    "Vpc": { "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.52.0.0/16" } },
    "SubnetA": {
      "Type": "AWS::EC2::Subnet",
      "Properties": {
        "VpcId": { "Ref": "Vpc" },
        "CidrBlock": "10.52.1.0/24",
        "AvailabilityZone": "us-east-1a"
      }
    },
    "FargateSecurityGroup": {
      "Type": "AWS::EC2::SecurityGroup",
      "Properties": {
        "GroupDescription": "the group the stack places its tasks in",
        "VpcId": { "Ref": "Vpc" }
      }
    },
    "LogGroup": {
      "Type": "AWS::Logs::LogGroup",
      "Properties": { "LogGroupName": "/ecs/flint-frames", "RetentionInDays": 7 }
    },
    "TaskRole": {
      "Type": "AWS::IAM::Role",
      "Properties": {
        "AssumeRolePolicyDocument": {
          "Version": "2012-10-17",
          "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole"
          }]
        }
      }
    },
    "ExecutionRole": {
      "Type": "AWS::IAM::Role",
      "Properties": {
        "AssumeRolePolicyDocument": {
          "Version": "2012-10-17",
          "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole"
          }]
        }
      }
    },
    "Cluster": {
      "Type": "AWS::ECS::Cluster",
      "Properties": { "ClusterName": "flint-frames-cluster" }
    },
    "TaskDefinition": {
      "Type": "AWS::ECS::TaskDefinition",
      "Properties": {
        "Family": "flint-frames",
        "Cpu": "256",
        "Memory": "512",
        "NetworkMode": "awsvpc",
        "RequiresCompatibilities": ["FARGATE"],
        "TaskRoleArn": { "Fn::GetAtt": ["TaskRole", "Arn"] },
        "ExecutionRoleArn": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
        "ContainerDefinitions": [{
          "Name": "frames",
          "Image": "public.ecr.aws/nginx/nginx:latest",
          "Essential": true,
          "LogConfiguration": {
            "LogDriver": "awslogs",
            "Options": {
              "awslogs-group": { "Ref": "LogGroup" },
              "awslogs-region": "us-east-1",
              "awslogs-stream-prefix": "frames"
            }
          }
        }]
      }
    },
    "Service": {
      "Type": "AWS::ECS::Service",
      "Properties": {
        "Cluster": { "Ref": "Cluster" },
        "TaskDefinition": { "Ref": "TaskDefinition" },
        "DesiredCount": 1,
        "LaunchType": "FARGATE",
        "NetworkConfiguration": {
          "AwsvpcConfiguration": {
            "AssignPublicIp": "ENABLED",
            "SecurityGroups": [{ "Ref": "FargateSecurityGroup" }],
            "Subnets": [{ "Ref": "SubnetA" }]
          }
        }
      }
    },
    "LatencyParameter": {
      "Type": "AWS::SSM::Parameter",
      "Properties": {
        "Name": "/flint/frames/latency-budget-ms",
        "Type": "String",
        "Value": "15000"
      }
    },
    "TraceGroup": {
      "Type": "AWS::XRay::Group",
      "Properties": {
        "GroupName": "flint-frames",
        "FilterExpression": "service(\"flint-frames\")"
      }
    },
    "ServiceLatencyAlarm": {
      "Type": "AWS::CloudWatch::Alarm",
      "Properties": {
        "AlarmName": { "Fn::Sub": "${AWS::StackName}-service-latency" },
        "AlarmDescription": "p90 frame latency over the declared budget",
        "Namespace": { "Ref": "Namespace" },
        "MetricName": "Time",
        "Dimensions": [
          { "Name": "Camera_name", "Value": "ALL" },
          { "Name": "Operation", "Value": "FrameQuartzLag" }
        ],
        "ExtendedStatistic": "p90",
        "Period": { "Ref": "AlarmPeriod" },
        "EvaluationPeriods": 3,
        "Threshold": 15000,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "notBreaching"
      }
    }
  },
  "Outputs": {
    "ClusterName": { "Value": { "Ref": "Cluster" } },
    "ServiceName": { "Value": { "Fn::GetAtt": ["Service", "Name"] } },
    "TaskDefinitionArn": { "Value": { "Ref": "TaskDefinition" } },
    "LogGroupName": { "Value": { "Ref": "LogGroup" } },
    "ParameterName": { "Value": { "Ref": "LatencyParameter" } },
    "TraceGroupName": { "Value": { "Ref": "TraceGroup" } },
    "SecurityGroupId": { "Value": { "Ref": "FargateSecurityGroup" } },
    "SubnetId": { "Value": { "Ref": "SubnetA" } },
    "ServiceLatencyAlarmName": { "Value": { "Ref": "ServiceLatencyAlarm" } }
  }
}"#;

/// The stack this suite deploys.
const STACK: &str = "flint-frames-estate";

/// The `MetricAlarm`s CloudWatch reports under `name`, asked for exactly as a client reading a
/// stack's alarm output asks: by name, for metric alarms.
async fn described_alarms(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let (status, document) = cloudwatch(
        router,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name], "AlarmTypes": ["MetricAlarm"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeAlarms, got {status} with {document}"
    );
    document
        .get("MetricAlarms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_monitored_fargate_estate_publishes_an_alarm_cloudwatch_holds() {
    let case = "a Fargate estate whose latency alarm the stack declares";
    let router = build_router();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", TEMPLATE),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = poll_to_terminal(&router, case, STACK).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete; reason {:?}; failed resources {:?}",
        opt_element(&described, "StackStatusReason"),
        failures_of(&router, STACK).await,
    );

    let outputs: BTreeMap<String, String> = outputs_of(&described);
    let name = outputs
        .get("ServiceLatencyAlarmName")
        .cloned()
        .unwrap_or_default();
    assert!(
        !name.is_empty(),
        "case {case:?}: expected the stack to publish the alarm's name, got {outputs:?}"
    );
    let alarms = described_alarms(&router, &name).await;
    assert_eq!(
        alarms.len(),
        1,
        "case {case:?}: expected CloudWatch to hold exactly the alarm {name:?} the stack \
         reports created, got {alarms:?}"
    );
    // And it is the alarm the template declared: the cadence its `AlarmPeriod` parameter
    // states, as the number CloudWatch's own `Period` member is modelled as.
    let alarm = alarms.into_iter().next().unwrap_or(Value::Null);
    let reported: Vec<(&str, Option<Value>)> = ["Period", "EvaluationPeriods", "ExtendedStatistic"]
        .into_iter()
        .map(|member| (member, alarm.get(member).cloned()))
        .collect();
    assert_eq!(
        reported,
        vec![
            ("Period", Some(json!(30))),
            ("EvaluationPeriods", Some(json!(3))),
            ("ExtendedStatistic", Some(json!("p90"))),
        ],
        "case {case:?}: expected the declared cadence, count and statistic, got {reported:?} \
         in {alarm}"
    );
}
