// SPDX-License-Identifier: CC0-1.0

//! A CloudFront management call, delivered on the account's CloudTrail API-call channel to a
//! rule whose targets are Step Functions state machines, starts an execution of the machines
//! the target ROLE admits and of no others.
//!
//! RULE: EventBridge makes a rule's state-machine target call acting as the role that target
//! names (`Target$RoleArn`: "the Amazon Resource Name (ARN) of the IAM role to be used for
//! this target when the rule is triggered"), so the delivery is made only where that role can
//! be assumed by this service for THIS rule and the policies in force on it allow
//! `states:StartExecution` — which makes several targets of ONE rule, matched by ONE event,
//! behave asymmetrically by role alone. The rule holds whatever channel the matched event
//! arrived on: while a trail of the Region is logging management events, every management call
//! the account serves is also published onto the DEFAULT bus as `AWS API Call via CloudTrail`
//! with `source` `aws.<service>`, and a global service's calls are recorded — and so published
//! — in the partition's leader Region, so a rule there sees them like any other event.
//!
//! Only a cross-service suite can pin this: CloudFront serves the call (restXml), CloudTrail
//! decides the account is delivering it (awsJson1_1), EventBridge matches the rule and makes
//! each handover (awsJson1_1), IAM holds the roles the handovers are made under (awsQuery) and
//! Step Functions holds the executions that did or did not start (awsJson1_0) — five services
//! over ONE engine, as `core/router/src/lib.rs` mounts them. The CHANNEL's own half of the
//! rule, over the patterns a queue target matches with, is
//! `crate::cloudfront_api_call_events`; this suite owns which TARGET of a matched event is
//! served.
//!
//! The rows differ along what the rule generalises over: the trust document the target's role
//! carries (this service alone, this service beside another, another service alone, this
//! service scoped by `aws:SourceArn` to the invoking rule, and scoped to a different rule of
//! the account), and what the policies in force on an assumable role allow (the target's own
//! call, or another of the same service). The rows are spread over two rules of identical
//! pattern — `PutTargets` takes at most [`TARGETS_PER_RULE`] targets for one rule — and each
//! rule carries both a row that must execute and rows that must not, so what differs between
//! the targets of one rule, matched by the SAME CloudFront call, is the role alone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every request below is signed for: CloudFront is global, so its management
/// calls are recorded — and published — in the partition's leader Region, and the rule that
/// matches them lives there.
const REGION: &str = "us-east-1";

/// CloudFront's API version, which every modelled route of it is under.
const API_VERSION: &str = "2020-05-31";

/// The channel the matched event arrives on.
const CLOUDFRONT_EVENT_SOURCE: &str = "aws.cloudfront";
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The service principal EventBridge assumes a target's role as, and one that is not this
/// service: a role trusting only the latter cannot be assumed for a handover here.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The call EventBridge makes on a state-machine target, which the target's role has to be
/// allowed, and another call of the same service, which is not it.
const START_EXECUTION: &str = "states:StartExecution";
const DESCRIBE_STATE_MACHINE: &str = "states:DescribeStateMachine";

/// The trail, its destination bucket, and the stem of the rules the targets below hang off.
const TRAIL: &str = "cloudfront-target-trust-trail";
const BUCKET: &str = "cloudfront-target-trust-logs";
const RULE_STEM: &str = "cloudfront-target-trust-rule";

/// How many targets one `PutTargets` installs on one rule: "a maximum of five targets per
/// rule", which is why the rows are spread over [`RULES`] rules of identical pattern rather
/// than hung off one.
const TARGETS_PER_RULE: usize = 5;

/// How many rules of identical pattern the rows are spread over.
const RULES: usize = 2;

/// How long an execution is waited for before a row fails, and how often it is looked at.
/// Generous: no row measures the deadline, only whether the handover was made at all.
const EXECUTION_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The state machine every target names a copy of: one `Pass` state, so what is asserted is
/// that the execution STARTED rather than anything it computed.
const PASS_DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// One target of the rule: the trust document its role carries, what that role is allowed,
/// and whether its state machine must hold an execution afterwards.
struct Row {
    case: &'static str,
    /// Names this row's role and state machine, so rows cannot collide.
    name: &'static str,
    /// Which of the identical rules carries this row's target.
    rule: usize,
    /// The service principals the role's trust policy names.
    trusts: &'static [&'static str],
    /// Which rule the trust policy's `aws:SourceArn` condition is scoped to, or `None` for an
    /// unconditional trust. A row scoped to a rule other than its own names a rule that
    /// really exists, so what bars the handover is the guard rather than an unresolvable ARN.
    scoped_to: Option<usize>,
    /// The action the role's identity policy allows.
    allows: &'static str,
    /// Whether this row's state machine must have an execution for the matched event.
    executes: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting this service, allowed the target's own call",
            name: "trusted",
            rule: 0,
            trusts: &[EVENTS_PRINCIPAL],
            scoped_to: None,
            allows: START_EXECUTION,
            executes: true,
        },
        Row {
            case: "a role trusting another service only",
            name: "mistrusted",
            rule: 0,
            trusts: &[OTHER_PRINCIPAL],
            scoped_to: None,
            allows: START_EXECUTION,
            executes: false,
        },
        Row {
            case: "a role whose trust is scoped to a different rule of the account",
            name: "trusted-for-another-rule",
            rule: 0,
            trusts: &[EVENTS_PRINCIPAL],
            scoped_to: Some(1),
            allows: START_EXECUTION,
            executes: false,
        },
        Row {
            case: "a role trusting this service beside another",
            name: "trusted-among-others",
            rule: 1,
            trusts: &[OTHER_PRINCIPAL, EVENTS_PRINCIPAL],
            scoped_to: None,
            allows: START_EXECUTION,
            executes: true,
        },
        Row {
            case: "a role whose trust is scoped to the rule that is invoking it",
            name: "trusted-for-this-rule",
            rule: 1,
            trusts: &[EVENTS_PRINCIPAL],
            scoped_to: Some(1),
            allows: START_EXECUTION,
            executes: true,
        },
        Row {
            case: "an assumable role allowed another call of the target's service",
            name: "unpermitted",
            rule: 1,
            trusts: &[EVENTS_PRINCIPAL],
            scoped_to: None,
            allows: DESCRIBE_STATE_MACHINE,
            executes: false,
        },
    ]
}

/// The five services this hop runs through, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region and routes the request.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves, as its status and body bytes.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host.clone())
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One `X-Amz-Target` RPC call that must answer 200, as its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> Value {
    let (status, answered) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", target),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {target} must answer 200, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One EventBridge call that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
    .await
}

/// One Step Functions call that must answer 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "states",
        &format!("AWSStepFunctions.{operation}"),
        "1.0",
        body,
    )
    .await
}

/// One CloudTrail call that must answer 200.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    rpc(
        router,
        case,
        "cloudtrail",
        &format!("CloudTrail_20131101.{operation}"),
        "1.1",
        body,
    )
    .await;
}

/// One `awsQuery` IAM action that must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = send(
        router,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: IAM {action} must answer 200, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// A trust policy naming `principals` for the assume, scoped to one rule's ARN when a rule is
/// named.
fn trust_policy(principals: &[&str], scoped_to: Option<usize>, account: &str) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": principals },
        "Action": "sts:AssumeRole",
    });
    if let Some(rule) = scoped_to {
        statement["Condition"] = json!({
            "ArnLike": { "aws:SourceArn": rule_arn(rule, account) },
        });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// An identity policy allowing `action` on every resource.
fn identity_policy(action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
    })
    .to_string()
}

/// The name the rule at `index` carries.
fn rule_name(index: usize) -> String {
    format!("{RULE_STEM}-{index}")
}

/// The ARN that rule carries in this Region.
fn rule_arn(index: usize, account: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{account}:rule/{}",
        rule_name(index)
    )
}

/// The role `name`, as the ARN it carries.
async fn given_role(router: &MultiServiceRouter, row: &Row, account: &str) -> String {
    iam(
        router,
        row.case,
        "CreateRole",
        &[
            ("RoleName", row.name),
            (
                "AssumeRolePolicyDocument",
                &trust_policy(row.trusts, row.scoped_to, account),
            ),
        ],
    )
    .await;
    iam(
        router,
        row.case,
        "PutRolePolicy",
        &[
            ("RoleName", row.name),
            ("PolicyName", "handover"),
            ("PolicyDocument", &identity_policy(row.allows)),
        ],
    )
    .await;
    format!("arn:aws:iam::{account}:role/{}", row.name)
}

/// The bucket a trail delivers its log files into, carrying the policy that lets CloudTrail
/// write there — created and granted exactly as a client does it, because CloudTrail resolves
/// the destination in the account's object store before it accepts the trail.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = send(router, "s3", "PUT", &format!("/{BUCKET}"), &[], Vec::new()).await;
    assert_eq!(
        status,
        200,
        "setup: the trail's bucket must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            "Condition": {
                "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
            },
        }],
    })
    .to_string();
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}?policy"),
        &[("content-type", "application/json")],
        policy.into_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "setup: the trail bucket's policy must be attached, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// The smallest `DistributionConfig` CloudFront accepts, which the one matched call creates.
fn distribution_config(reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Comment>target role trust</Comment>\
         <Enabled>true</Enabled>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>origin.example.com</DomainName>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior>\
         <TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>\
         </DistributionConfig>"
    )
}

/// How many executions `machine` holds.
async fn execution_count(router: &MultiServiceRouter, case: &str, machine: &str) -> usize {
    let listed = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": machine }),
    )
    .await;
    listed["executions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

#[tokio::test]
async fn an_api_call_events_state_machine_targets_follow_their_roles_trust() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let rows = rows();

    // A trail of this Region, logging: the condition under which the account publishes the
    // management calls it serves onto the default bus.
    given_bucket(&router).await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({ "Name": TRAIL, "S3BucketName": BUCKET }),
    )
    .await;
    cloudtrail(&router, "setup", "StartLogging", &json!({ "Name": TRAIL })).await;

    // The rules, on the DEFAULT bus — the only bus an AWS-published event reaches — all of
    // one pattern, so the same published event matches every one of them.
    for index in 0..RULES {
        events(
            &router,
            "setup",
            "PutRule",
            &json!({
                "Name": rule_name(index),
                "State": "ENABLED",
                "EventPattern": json!({
                    "source": [CLOUDFRONT_EVENT_SOURCE],
                    "detail-type": [API_CALL_DETAIL_TYPE],
                })
                .to_string(),
            }),
        )
        .await;
    }

    // One target per row: that row's state machine, under that row's role, on the rule the
    // row names.
    let mut machines = Vec::new();
    let mut targets: Vec<Vec<Value>> = vec![Vec::new(); RULES];
    for row in &rows {
        let role_arn = given_role(&router, row, account).await;
        let created = sfn(
            &router,
            row.case,
            "CreateStateMachine",
            &json!({
                "name": row.name,
                "definition": PASS_DEFINITION,
                "roleArn": format!("arn:aws:iam::{account}:role/states-execution"),
            }),
        )
        .await;
        let machine = created["stateMachineArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a state machine ARN, got {created}",
                    row.case
                )
            })
            .to_string();
        targets[row.rule].push(json!({ "Id": row.name, "Arn": machine, "RoleArn": role_arn }));
        machines.push(machine);
    }
    for (index, installing) in targets.iter().enumerate() {
        assert!(
            installing.len() <= TARGETS_PER_RULE,
            "case {:?}: one rule takes at most {TARGETS_PER_RULE} targets, got {}",
            rule_name(index),
            installing.len()
        );
        let installed = events(
            &router,
            "setup",
            "PutTargets",
            &json!({ "Rule": rule_name(index), "Targets": installing }),
        )
        .await;
        assert_eq!(
            installed["FailedEntryCount"],
            json!(0),
            "setup: every target of {:?} must be installed, got {installed}",
            rule_name(index)
        );
    }

    // ONE CloudFront management call, which every rule above matches on the API-call channel:
    // the role each target names is the only thing that differs between the handovers it
    // drives.
    let (status, body) = send(
        &router,
        "cloudfront",
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        distribution_config("ref-target-role-trust").into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        201,
        "setup: CreateDistribution must answer 201, got {status} {}",
        String::from_utf8_lossy(&body)
    );

    // The rows that deliver, FIRST: their execution is the barrier the silent rows are judged
    // against, so an empty listing is an absence rather than a handover still in flight.
    for (row, machine) in rows.iter().zip(&machines).filter(|(row, _)| row.executes) {
        let deadline = Instant::now() + EXECUTION_DEADLINE;
        loop {
            let started = execution_count(&router, row.case, machine).await;
            if started > 0 {
                assert_eq!(
                    started, 1,
                    "case {:?}: one matched event starts one execution, got {started}",
                    row.case
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the target's state machine to hold an execution for the \
                 matched event, got none",
                row.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    for (row, machine) in rows.iter().zip(&machines).filter(|(row, _)| !row.executes) {
        let started = execution_count(&router, row.case, machine).await;
        assert_eq!(
            started, 0,
            "case {:?}: a handover the target's role bars starts no execution, got {started}",
            row.case
        );
    }
}
