// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED Application Load Balancer with its access logs switched on delivers into the
//! bucket, once a client has sent it a request on the address it published.
//!
//! RULE: a load balancer whose `access_logs.s3.*` attributes name a destination the delivery's
//! own write is granted delivers into it — the file it verifies the destination with when the
//! delivery is switched on, and then one gzipped RECORD under
//! `AWSLogs/<account-id>/elasticloadbalancing/<region>/` per request it answered — and the
//! client is answered by the TARGET, not by the load balancer, whichever way the destination's
//! grant is spelled.
//!
//! This is the second half of the sibling suite
//! `elbv2_stack_enabled_access_logs_verify_their_destination`, which spans the SPELLINGS of that
//! grant over the deployment alone: here one of those destinations is driven end to end, so the
//! record of an answered request is delivered through the same seam the verification file is.
//! Four crates have to agree for it to hold — CloudFormation deploys the estate, Lambda runs the
//! target, Elastic Load Balancing forwards to it and delivers the record, and Amazon S3 holds
//! the destination — so the suite lives here, over ONE engine, assembled as the binary assembles
//! it.
//!
//! The suite runs LAST (its `zz_` name) because it dials a socket the deployment itself opened:
//! the address a load balancer publishes is reached as a client handed that address reaches it.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE dial of an address a load balancer PUBLISHED this test crate keeps — a real connection
// to the authority, whose port it first holds to being one of the emulator's own.
use crate::elbv2_published_address_enforces_its_web_acl::exchange;
// The emulator this pair of suites assembles, the wire the deployment is driven over and the
// failures a deployment reports, all from the sibling suite that spans the grant's spellings: one
// assembly and one caller for the two, so they cannot drift.
use crate::elbv2_stack_enabled_access_logs_verify_their_destination::{
    authorization, build_router, cloudformation, reported_failures,
};
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a step waits for the condition it needs — the stack's terminal status, the answer
/// the published address gives, the record the delivery makes — and how often it looks again.
///
/// Every wait below waits for its CONDITION; the deadline is only the backstop that turns
/// something nothing will ever satisfy into a readable failure instead of a hang.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The stack this suite deploys, and the bucket its load balancer delivers into.
const STACK: &str = "elbv2-logged-estate";
const BUCKET: &str = "elbv2-logged-estate-logs";

/// The path the client asks for, and what the target answers it with.
const SERVED_PATH: &str = "/health";
const SERVED_BODY: &str = "ok";

/// The segments of the delivered layout a client reads the destination under: the segment the
/// delivery writes into every key, the name of the file it verifies the destination with, and
/// the segment naming the service whose records a folder holds.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";
const TEST_FILE_NAME: &str = "ELBAccessLogTestFile";
const LOG_FILE_SERVICE_SEGMENT: &str = "elasticloadbalancing";

/// Every key the destination holds under the layout's own segment, read through `ListObjectsV2`
/// — the probe a client polling for the delivery makes.
async fn delivered(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let host = format!("s3.{REGION}.amazonaws.com");
    let path = format!(
        "/{BUCKET}?list-type=2&prefix={}",
        form_encode(&format!("{DELIVERED_LOGS_SEGMENT}/"))
    );
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: ListObjectsV2({BUCKET:?}) must answer 200, got {status} with {body}"
    );
    elements(&body, "Key")
}

/// The template this suite deploys: the destination bucket carrying the published log-delivery
/// grant, the network, the load balancer with its access logs switched on, and the `lambda`
/// target group its listener forwards to.
fn template() -> String {
    format!(
        "Resources:
  LogBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: {BUCKET}
  LogBucketPolicy:
    Type: AWS::S3::BucketPolicy
    Properties:
      Bucket: !Ref LogBucket
      PolicyDocument:
        Version: \"2012-10-17\"
        Statement:
          - Effect: Allow
            Principal:
              Service: delivery.logs.amazonaws.com
            Action: s3:PutObject
            Resource: !Sub \"arn:aws:s3:::${{LogBucket}}/AWSLogs/${{AWS::AccountId}}/*\"
            Condition:
              StringEquals:
                s3:x-amz-acl: bucket-owner-full-control
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.11.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.11.1.0/24
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.11.2.0/24
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
    DependsOn: LogBucketPolicy
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
  FnRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: \"2012-10-17\"
        Statement:
          - Effect: Allow
            Principal:
              Service: lambda.amazonaws.com
            Action: sts:AssumeRole
  Fn:
    Type: AWS::Lambda::Function
    Properties:
      Handler: index.handler
      Runtime: python3.12
      Role: !GetAtt FnRole.Arn
      Code:
        ZipFile: |
          def handler(event, context):
              return {{
                  \"statusCode\": 200,
                  \"statusDescription\": \"200 OK\",
                  \"isBase64Encoded\": False,
                  \"headers\": {{\"Content-Type\": \"text/plain\"}},
                  \"body\": \"{SERVED_BODY}\",
              }}
  Permission:
    Type: AWS::Lambda::Permission
    Properties:
      FunctionName: !GetAtt Fn.Arn
      Action: lambda:InvokeFunction
      Principal: elasticloadbalancing.amazonaws.com
  Tg:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    DependsOn: Permission
    Properties:
      Name: elbv2-logged-estate-tg
      TargetType: lambda
      Targets:
        - Id: !GetAtt Fn.Arn
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: 80
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref Tg
Outputs:
  AlbDns:
    Value: !GetAtt Alb.DNSName
"
    )
}

/// RULE: the destination of a deployed load balancer whose logs are on holds the file that
/// verifies it and, once the load balancer has answered a request on the address it published,
/// the record of that request.
#[tokio::test(flavor = "multi_thread")]
async fn a_logged_estate_delivers_the_record_of_the_request_its_target_answered() {
    let case = "a routed request through the deployed load balancer";
    let router = build_router();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {body}"
    );

    let started = Instant::now();
    let (reported, described) = loop {
        let (status, body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || started.elapsed() >= SETTLE_DEADLINE {
            break (reported, body);
        }
        tokio::time::sleep(LOOK).await;
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the deployment must complete, got {reported:?}; the failures it \
         reported: {:?}",
        reported_failures(&router, STACK).await
    );

    // The destination was VERIFIED by the deployment's own attribute write, before any request
    // was answered: the one file Elastic Load Balancing writes for that and nothing else.
    let held = delivered(&router, case).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: enabling the delivery at deploy must leave exactly the file that \
         verifies the destination, got {held:?}"
    );
    assert!(
        held[0].ends_with(&format!("/{TEST_FILE_NAME}")),
        "case {case:?}: expected {DELIVERED_LOGS_SEGMENT}/<account-id>/{TEST_FILE_NAME}, got {:?}",
        held[0]
    );

    // The client is answered by the TARGET, on the address the deployment published.
    let published = outputs_of(&described)
        .get("AlbDns")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no AlbDns, got {described}"));
    let started = Instant::now();
    let answered = loop {
        let answered = exchange(case, &published, "GET", SERVED_PATH, &[]).await;
        if answered.status == 200 || started.elapsed() >= SETTLE_DEADLINE {
            break answered;
        }
        tokio::time::sleep(LOOK).await;
    };
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the target's own answer on {published}{SERVED_PATH}, got {:?}",
        answered.raw
    );
    assert!(
        answered.raw.contains(SERVED_BODY),
        "case {case:?}: expected the target's body {SERVED_BODY:?}, got {:?}",
        answered.raw
    );

    // ...and the record of that request is delivered beside the verification file, under the
    // folder the layout keys this service's records by.
    let folder = format!("/{LOG_FILE_SERVICE_SEGMENT}/{REGION}/");
    let started = Instant::now();
    let records = loop {
        let records: Vec<String> = delivered(&router, case)
            .await
            .into_iter()
            .filter(|key| key.contains(&folder))
            .collect();
        if !records.is_empty() || started.elapsed() >= SETTLE_DEADLINE {
            break records;
        }
        tokio::time::sleep(LOOK).await;
    };
    assert!(
        !records.is_empty(),
        "case {case:?}: expected the record of the answered request under \
         {DELIVERED_LOGS_SEGMENT}/<account-id>{folder}, got {:?}",
        delivered(&router, case).await
    );
}
