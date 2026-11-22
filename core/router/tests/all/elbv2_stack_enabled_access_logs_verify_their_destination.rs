// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED Application Load Balancer leaves in the Amazon S3 bucket its stack told it
//! to deliver its access logs into.
//!
//! RULE: a stack whose `AWS::ElasticLoadBalancingV2::LoadBalancer` carries the
//! `access_logs.s3.*` attributes that switch delivery on reaches `CREATE_COMPLETE` whenever the
//! destination bucket's policy admits the delivery's own write — whichever spelling of the
//! published log-delivery grant that policy is written in, including one conditioned on the
//! facts that write carries — and the load balancer then VERIFIES the destination, so a client
//! listing the bucket under the layout's own segment finds exactly the one file Elastic Load
//! Balancing writes for that: `AWSLogs/<account-id>/ELBAccessLogTestFile` for a destination
//! whose prefix attribute is empty.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation drives the deployment and applies the declared attributes through Elastic
//! Load Balancing's own `ModifyLoadBalancerAttributes`, Elastic Load Balancing judges the
//! destination and writes into it, and Amazon S3 owns the bucket, the policy that admits the
//! write and the listing the client reads. Everything runs over ONE engine, assembled exactly
//! as the binary assembles it, so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: how the published grant is SPELLED
//! (the whole bucket or just this account's delivery folder, the log-delivery service principal
//! or the delivery account's root, with and without the condition that the written object is
//! handed to the bucket's owner), and whether the load balancer's declaration waits for the
//! policy at all.

use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `StackStatus` a deployment of this estate must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a row waits for the stack operation to settle, and how often it looks again.
///
/// The wait is for the CONDITION — a status that is no longer `*_IN_PROGRESS` — which the loop
/// polls for; the deadline is only the backstop that turns something nothing will ever satisfy
/// into a readable failure instead of a hang, so it is generous.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The segment the delivery itself writes into every key, which is also what a client lists the
/// destination under, and the name Elastic Load Balancing gives the file it verifies a
/// destination with.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";
const TEST_FILE_NAME: &str = "ELBAccessLogTestFile";

/// The whole emulator over one engine, exactly as the binary assembles it.
///
/// `pub(crate)` because the sibling suite that drives one of these deployments end to end
/// (`zz_elbv2_access_log_stack_probe`) assembles the emulator and calls the two services'
/// wires the same way: ONE assembly and ONE caller for the pair, so they cannot drift.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
pub(crate) fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One CloudFormation call over its awsQuery wire, answered with its status and the XML.
pub(crate) async fn cloudformation(
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
    let response = Box::pin(router.dispatch(request)).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One request against Amazon S3's REST surface, path-style — what a client reading the
/// destination sends.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Every resource failure the stack reported, which is where the owning service's own refusal of
/// a resource is carried — quoted by a row whose deployment did not complete.
pub(crate) async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    if status != 200 {
        return vec![format!("DescribeStackEvents answered {status}: {body}")];
    }
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let resource_status = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .unwrap_or_default();
            if !resource_status.contains("FAILED") {
                return None;
            }
            let logical = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {resource_status}: {reason}"))
        })
        .collect()
}

/// One row: the stack it deploys, the bucket the logs are sent to, how the load balancer's
/// declaration is ORDERED against the policy, and the `Statement` list the policy carries.
struct Row {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    /// The `DependsOn` the load balancer declares, which is empty for a declaration that waits
    /// for nothing.
    depends_on: &'static str,
    statement: &'static str,
}

/// The template a row deploys: the destination bucket and its policy, the network the load
/// balancer is placed in, and the load balancer whose declared attributes switch its access-log
/// delivery on with no prefix at all.
fn template(row: &Row) -> String {
    let Row {
        bucket,
        depends_on,
        statement,
        ..
    } = row;
    format!(
        "Resources:
  LogBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: {bucket}
  LogBucketPolicy:
    Type: AWS::S3::BucketPolicy
    Properties:
      Bucket: !Ref LogBucket
      PolicyDocument:
        Version: \"2012-10-17\"
        Statement:
{statement}
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.9.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.9.1.0/24
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.9.2.0/24
      AvailabilityZone: {REGION}b
  AlbSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group the load balancer is placed in
      VpcId: !Ref Vpc
      SecurityGroupIngress:
        - IpProtocol: tcp
          FromPort: 80
          ToPort: 80
          CidrIp: 0.0.0.0/0
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
{depends_on}
    Properties:
      Type: application
      Scheme: internal
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
      SecurityGroups:
        - !Ref AlbSecurityGroup
      LoadBalancerAttributes:
        - Key: access_logs.s3.enabled
          Value: \"true\"
        - Key: access_logs.s3.bucket
          Value: !Ref LogBucket
        - Key: access_logs.s3.prefix
          Value: \"\"
"
    )
}

/// RULE: a deployment that switches a load balancer's access logs on into a bucket whose policy
/// admits the delivery's own write completes, and leaves exactly the file the delivery verifies
/// the destination with.
#[tokio::test(flavor = "multi_thread")]
async fn a_stack_that_enables_access_logs_leaves_the_delivery_test_file() {
    let rows: &[Row] = &[
        Row {
            case: "the whole bucket granted, with the load balancer waiting for the grant",
            stack: "elbv2-logs-waiting",
            bucket: "elbv2-logs-waiting",
            depends_on: "    DependsOn: LogBucketPolicy",
            statement: "          - Effect: Allow
            Principal:
              Service: logdelivery.elasticloadbalancing.amazonaws.com
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${LogBucket}/*\"",
        },
        Row {
            case: "the same grant, with a declaration that waits for nothing",
            stack: "elbv2-logs-unordered",
            bucket: "elbv2-logs-unordered",
            depends_on: "",
            statement: "          - Effect: Allow
            Principal:
              Service: logdelivery.elasticloadbalancing.amazonaws.com
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${LogBucket}/*\"",
        },
        Row {
            case: "the grant scoped to this account's own delivery folder",
            stack: "elbv2-logs-scoped",
            bucket: "elbv2-logs-scoped",
            depends_on: "    DependsOn: LogBucketPolicy",
            statement: "          - Effect: Allow
            Principal:
              Service: logdelivery.elasticloadbalancing.amazonaws.com
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${LogBucket}/AWSLogs/${AWS::AccountId}/*\"",
        },
        Row {
            case: "the grant conditioned on the object being handed to the bucket's owner",
            stack: "elbv2-logs-conditioned",
            bucket: "elbv2-logs-conditioned",
            depends_on: "    DependsOn: LogBucketPolicy",
            statement: "          - Effect: Allow
            Principal:
              Service: delivery.logs.amazonaws.com
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${LogBucket}/AWSLogs/${AWS::AccountId}/*\"
            Condition:
              StringEquals:
                s3:x-amz-acl: bucket-owner-full-control",
        },
        Row {
            case: "the grant made to the delivery account's own root",
            stack: "elbv2-logs-account",
            bucket: "elbv2-logs-account",
            depends_on: "    DependsOn: LogBucketPolicy",
            statement: "          - Effect: Allow
            Principal:
              AWS: arn:aws:iam::797873946194:root
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${LogBucket}/AWSLogs/${AWS::AccountId}/*\"",
        },
    ];

    for row in rows {
        let Row {
            case,
            stack,
            bucket,
            ..
        } = row;
        // One emulator per row, so the buckets, the networks and the load balancers of one row
        // are never what another row's deployment found.
        let router = build_router();
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", stack), ("TemplateBody", &template(row))],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must be accepted, got {status} with {body}"
        );

        let started = std::time::Instant::now();
        let reported = loop {
            let (status, body) =
                cloudformation(&router, "DescribeStacks", &[("StackName", stack)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
            );
            let reported = opt_element(&body, "StackStatus").unwrap_or_default();
            if !reported.ends_with("_IN_PROGRESS") || started.elapsed() >= SETTLE_DEADLINE {
                break reported;
            }
            tokio::time::sleep(LOOK).await;
        };
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: the deployment must complete, got {reported:?}; the failures it \
             reported: {:?}",
            reported_failures(&router, stack).await
        );

        let (status, listed) = s3(
            &router,
            "GET",
            &format!(
                "/{bucket}?list-type=2&prefix={}",
                form_encode(&format!("{DELIVERED_LOGS_SEGMENT}/"))
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListObjectsV2({bucket:?}) must answer 200, got {status} with {listed}"
        );
        let held = elements(&listed, "Key");
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: enabling the delivery at deploy must leave exactly the file that \
             verifies the destination, got {held:?}"
        );
        let key = &held[0];
        assert!(
            key.starts_with(&format!("{DELIVERED_LOGS_SEGMENT}/"))
                && key.ends_with(&format!("/{TEST_FILE_NAME}")),
            "case {case:?}: expected {DELIVERED_LOGS_SEGMENT}/<account-id>/{TEST_FILE_NAME}, got \
             {key:?}"
        );
    }
}
