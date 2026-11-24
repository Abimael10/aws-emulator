// SPDX-License-Identifier: CC0-1.0

//! A REPAIR to the S3 -> EventBridge -> Lambda -> CloudFront wiring decides the NEXT object
//! event: the write made after the repair invalidates, the one made before it does not.
//!
//! RULE: every hop of a published event is decided when the event is published, from the state
//! the deciding service holds at that instant — the bucket's `EventBridgeConfiguration`, the
//! rule's own `EventPattern` and `State`, and the target function's resource-based policy — so
//! a wiring fault corrected after an event was published withholds that event and admits the
//! next one, with no re-wiring of anything else and nothing re-delivered.
//!
//! Only a cross-service suite can state this: the event is Amazon S3's, the pattern match and
//! the handover EventBridge's, the resource policy and the handler Lambda's, the execution role
//! IAM's, and the invalidation the running handler submits CloudFront's.
//! [`super::s3_object_created_invalidates_the_distribution_edge`] pins the chain itself with a
//! permitted and an un-permitted target of ONE rule, and
//! [`super::events_rule_target_handler_invalidates_its_distribution`] pins a rule's target
//! shapings — neither states what a correction applied to a LIVE chain does to the events on
//! either side of it, which is what a client repairing a misconfigured pipeline sees.
//!
//! The rows differ along WHICH hop is broken and then repaired — the function's resource policy
//! (no statement at all), the rule's pattern (scoped to another bucket's ARN), and the rule's
//! `State` (`DISABLED`) — because the rule above is about the chain's decision points, not
//! about one of them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    authorization, build_router, cloudfront, deployed, s3, wait_until_active, API_VERSION, REGION,
};
use crate::query_wire::{element, elements};

/// Where Lambda's function routes are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The service principal EventBridge hands a function target over as, and the action a grant
/// for it has to name.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `source` and `detail-type` of the event a write into an EventBridge-enabled bucket
/// publishes.
const S3_SOURCE: &str = "aws.s3";
const OBJECT_CREATED: &str = "Object Created";

/// The S3 document namespace a notification configuration is written in.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long the chain's observable end state is waited for, and how long the half that must
/// observe NOTHING keeps looking before concluding nothing came. Generous: no row measures a
/// deadline, only the outcome. The quiet window rests on the re-check after the repair as
/// well — an invalidation that arrived late would be found by it — so neither half of a row
/// depends on how fast this host runs.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);
const QUIET_BUDGET: Duration = Duration::from_secs(5);

/// How often a wait looks again. Every look is a pair of CloudFront reads, so the interval is
/// what keeps a wait from loading the host it shares with the rest of this test target.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

/// The handler every function here runs: it reads the written key out of the event it was
/// handed and submits an invalidation for the path it makes of it, through the AWS SDK the
/// managed runtime supplies.
const INVALIDATING_HANDLER: &str = r#"
import boto3

DISTRIBUTION = "__DISTRIBUTION__"

CLIENT = boto3.client("cloudfront")


def lambda_handler(event, context):
    key = event["detail"]["object"]["key"]
    submitted = CLIENT.create_invalidation(
        DistributionId=DISTRIBUTION,
        InvalidationBatch={
            "Paths": {"Items": ["/" + key]},
            "CallerReference": event["id"],
        },
    )
    return {"invalidation": submitted["Invalidation"]["Id"]}
"#;

/// The ARN of a bucket, which is what an `aws.s3` event's envelope lists under `resources`.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// One Lambda `restJson1` call, answered with the status and the body as text.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &str) -> (u16, String) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("events.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("authorization", authorization("events"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// `s3:CreateBucket` followed by the `PUT /<bucket>?notification` that "enables delivery of
/// events to Amazon EventBridge" for it (`NotificationConfiguration$EventBridgeConfiguration`).
async fn create_event_bridge_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">\
         <EventBridgeConfiguration></EventBridgeConfiguration>\
         </NotificationConfiguration>"
    );
    let stored = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        stored.status, 200,
        "case {case:?}: enabling EventBridge notifications on {bucket:?} must succeed, got {} \
         with {}",
        stored.status, stored.body
    );
}

/// `s3:PutObject`, the write whose event the chain carries.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let written = s3(router, "PUT", &format!("/{bucket}/{key}"), "the published asset").await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject({key:?}) must succeed, got {} with {}",
        written.status, written.body
    );
}

/// `cloudfront:CreateDistribution` over `bucket` as a plain S3 origin, answering its id. No row
/// fetches through this distribution: what they read is the INVALIDATION state the chain leaves.
async fn create_distribution(router: &MultiServiceRouter, bucket: &str) -> String {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>s3-eventbridge-pipeline-repair</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id>\
         <DomainName>{bucket}.s3.{REGION}.amazonaws.com</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <ForwardedValues><QueryString>false</QueryString>\
         <Cookies><Forward>none</Forward></Cookies></ForwardedValues>\
         <MinTTL>0</MinTTL>\
         </DefaultCacheBehavior>\
         <Comment>the distribution an object write invalidates</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let created = cloudfront(router, "POST", &format!("/{API_VERSION}/distribution"), &body).await;
    assert_eq!(
        created.status, 201,
        "setup: CreateDistribution must succeed, got {} with {}",
        created.status, created.body
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "setup: the created distribution reports no Id: {}",
        created.body
    );
    id
}

/// The stack that declares the execution role every handler runs as and one function per row:
/// the role trusts Lambda, which is what `CreateFunction` checks, and carries the invalidation
/// grants the handler's own CloudFront call is judged against.
fn chain_stack(stack: &str, distribution: &str, functions: &[String]) -> String {
    let mut resources = Map::new();
    resources.insert(
        "InvalidatorRole".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{stack}-exec"),
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                },
                "Policies": [{
                    "PolicyName": "invalidate-the-distribution",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["cloudfront:CreateInvalidation"],
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }),
    );
    for (index, function) in functions.iter().enumerate() {
        resources.insert(
            format!("Function{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["InvalidatorRole", "Arn"] },
                    "Timeout": 30,
                    "Code": {
                        "ZipFile": INVALIDATING_HANDLER.replace("__DISTRIBUTION__", distribution),
                    },
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// The ARN Lambda holds for `name`, read back from the service rather than built here.
async fn function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = lambda(router, "GET", &format!("{FUNCTIONS_URI}/{name}"), "").await;
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    parsed["Configuration"]["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup: GetFunction({name:?}) answered {status} with no FunctionArn: {body}")
        })
        .to_string()
}

/// `lambda:AddPermission` for the EventBridge service principal, scoped to `rule_arn` — the
/// statement a rule's handover of a matched event to a function target needs ("for Lambda
/// functions … EventBridge relies on resource-based policies", `PutTargets`).
async fn grant_events_invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    rule_arn: &str,
) {
    let (status, body) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        &json!({
            "StatementId": "events-invoke",
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission({function:?}) must answer 201, got {status} with {body}"
    );
}

/// Every invalidation CloudFront holds for `distribution`, as the paths its batch named keyed
/// by the batch's own `CallerReference` — read through the service's own `ListInvalidations` and
/// `GetInvalidation`, which is where an invalidation has to exist.
async fn invalidations(
    router: &MultiServiceRouter,
    distribution: &str,
) -> BTreeMap<String, Vec<String>> {
    let listed = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution}/invalidation"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "ListInvalidations({distribution}) must answer 200, got {} with {}",
        listed.status, listed.body
    );
    let mut held = BTreeMap::new();
    for id in elements(&listed.body, "Id") {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{distribution}/invalidation/{id}"),
            "",
        )
        .await;
        assert_eq!(
            read.status, 200,
            "GetInvalidation({id}) must answer 200, got {} with {}",
            read.status, read.body
        );
        held.insert(element(&read.body, "CallerReference"), elements(&read.body, "Path"));
    }
    held
}

/// Look for an invalidation of `distribution` whose batch names exactly `[path]` for as long as
/// `within`, answering as soon as one is held — and `None` when none ever is.
async fn invalidation_for(
    router: &MultiServiceRouter,
    distribution: &str,
    path: &str,
    within: Duration,
) -> Option<Vec<String>> {
    let wanted = vec![path.to_string()];
    let deadline = Instant::now() + within;
    loop {
        let held = invalidations(router, distribution).await;
        if let Some(found) = held.into_values().find(|paths| *paths == wanted) {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Which hop of the chain a row breaks and then repairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The function carries no resource-based statement for the EventBridge principal, so the
    /// rule's handover is withheld by the function's own policy; the repair is `AddPermission`.
    NoInvokeGrant,
    /// The rule's pattern names ANOTHER bucket's ARN under `resources`, so this bucket's event
    /// matches nothing; the repair is a `PutRule` carrying the bucket's own ARN.
    PatternNamesAnotherBucket,
    /// The rule is `DISABLED`, so it matches nothing at all ("the rule … is not actually
    /// triggered", `PutRule$State`); the repair is `EnableRule`.
    RuleDisabled,
}

/// One row: the bucket, the rule and the function it drives, and the fault it repairs.
struct Row {
    case: &'static str,
    bucket: &'static str,
    rule: &'static str,
    /// The key written BEFORE the repair, whose event the broken chain must withhold, and the
    /// one written after it, whose event the repaired chain must carry.
    before: &'static str,
    after: &'static str,
    fault: Fault,
}

const ROWS: &[Row] = &[
    Row {
        case: "the target function's resource policy withholds the EventBridge principal",
        bucket: "pipeline-repair-no-grant",
        rule: "pipeline-repair-no-grant-rule",
        before: "withheld/app.js",
        after: "granted/app.js",
        fault: Fault::NoInvokeGrant,
    },
    Row {
        case: "the rule's pattern names another bucket's ARN",
        bucket: "pipeline-repair-wrong-arn",
        rule: "pipeline-repair-wrong-arn-rule",
        before: "unmatched/index.html",
        after: "matched/index.html",
        fault: Fault::PatternNamesAnotherBucket,
    },
    Row {
        case: "the rule is disabled",
        bucket: "pipeline-repair-disabled",
        rule: "pipeline-repair-disabled-rule",
        before: "while-disabled/site.css",
        after: "once-enabled/site.css",
        fault: Fault::RuleDisabled,
    },
];

/// The bucket ARN a row's rule names while it is broken: one no bucket of this suite has, so
/// the pattern is scoped to a resource the published event cannot carry.
const ANOTHER_BUCKET: &str = "pipeline-repair-some-other-bucket";

/// A fault in any one hop of the chain withholds the event published while it stands, and the
/// repair carries the next one all the way to the distribution's invalidation.
#[tokio::test]
async fn a_repaired_chain_carries_the_next_object_event() {
    let router = build_router();

    for row in ROWS {
        create_event_bridge_bucket(&router, row.case, row.bucket).await;
    }

    let distribution = create_distribution(&router, ROWS[0].bucket).await;

    let functions: Vec<String> = ROWS
        .iter()
        .map(|row| format!("{}-invalidator", row.bucket))
        .collect();
    let stack = "xsvc-s3-eventbridge-pipeline-repair";
    deployed(
        &router,
        stack,
        &chain_stack(stack, &distribution, &functions),
    )
    .await;
    for function in &functions {
        wait_until_active(&router, function).await;
    }

    for (row, function) in ROWS.iter().zip(&functions) {
        let case = row.case;
        let matched_arn = bucket_arn(row.bucket);
        let pattern = |resource: &str| {
            json!({
                "source": [S3_SOURCE],
                "detail-type": [OBJECT_CREATED],
                "resources": [resource],
            })
            .to_string()
        };

        // The chain as the row leaves it BROKEN: the rule, its target, and the grant every row
        // but the first one carries.
        let broken_resource = if row.fault == Fault::PatternNamesAnotherBucket {
            bucket_arn(ANOTHER_BUCKET)
        } else {
            matched_arn.clone()
        };
        let state = if row.fault == Fault::RuleDisabled {
            "DISABLED"
        } else {
            "ENABLED"
        };
        let ruled = events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": row.rule,
                "EventPattern": pattern(&broken_resource),
                "State": state,
            }),
        )
        .await;
        let rule_arn = ruled["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: PutRule reports no RuleArn: {ruled}"))
            .to_string();

        let target_arn = function_arn(&router, function).await;
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": row.rule,
                "Targets": [{ "Id": "invalidator", "Arn": target_arn }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: the target must be accepted, got {wired}"
        );
        if row.fault != Fault::NoInvokeGrant {
            grant_events_invoke(&router, case, function, &rule_arn).await;
        }

        // The write made while the fault stands reaches no handler, so nothing is invalidated
        // for it. This window is a BOUND on the wait, not a measurement: a delivery that
        // arrived after it would still be caught by the re-check at the end of the row, which
        // is why that re-check has to stay.
        put_object(&router, case, row.bucket, row.before).await;
        let withheld_path = format!("/{}", row.before);
        let withheld =
            invalidation_for(&router, &distribution, &withheld_path, QUIET_BUDGET).await;
        assert_eq!(
            withheld, None,
            "case {case:?}: the chain is broken, so the write of {:?} must invalidate nothing, \
             got {withheld:?}",
            row.before
        );

        // The repair, which touches ONLY the hop the row broke.
        match row.fault {
            Fault::NoInvokeGrant => grant_events_invoke(&router, case, function, &rule_arn).await,
            Fault::PatternNamesAnotherBucket => {
                events(
                    &router,
                    case,
                    "PutRule",
                    &json!({
                        "Name": row.rule,
                        "EventPattern": pattern(&matched_arn),
                        "State": "ENABLED",
                    }),
                )
                .await;
            }
            Fault::RuleDisabled => {
                events(&router, case, "EnableRule", &json!({ "Name": row.rule })).await;
            }
        }

        // BACKED: the invalidation is CloudFront's own record, read through CloudFront's own
        // API rather than out of anything the handler said.
        put_object(&router, case, row.bucket, row.after).await;
        let wanted = format!("/{}", row.after);
        let carried = invalidation_for(&router, &distribution, &wanted, DELIVERY_BUDGET).await;
        assert_eq!(
            carried,
            Some(vec![wanted.clone()]),
            "case {case:?}: the repaired chain must invalidate {wanted:?} for the write of {:?}, \
             got {carried:?}",
            row.after
        );

        // The withheld event is not re-delivered by the repair: what it would have invalidated
        // is still absent.
        let still = invalidation_for(&router, &distribution, &withheld_path, Duration::ZERO).await;
        assert_eq!(
            still, None,
            "case {case:?}: the repair must not re-deliver the event published before it, got \
             {still:?}"
        );
    }
}
