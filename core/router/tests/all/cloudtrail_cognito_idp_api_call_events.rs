// SPDX-License-Identifier: CC0-1.0

//! The CloudTrail-sourced management-event fan-out of a user-pool control plane, reaching
//! the EventBridge rules a client writes over its administrative writes.
//!
//! RULE: while a trail of the Region is logging management events of a call's read/write
//! class, every control-plane call the account serves — whichever service serves it — is
//! ALSO published onto that Region's DEFAULT event bus, with `source`
//! `aws.<endpoint-prefix>` of the serving service, `detail-type`
//! `AWS API Call via CloudTrail`, `account` and `region` of the call, and the CloudTrail
//! record of the call as `detail`; a rule naming `source`, `detail-type`,
//! `detail.eventSource` or `detail.eventName` is handed exactly the events matching it and
//! nothing else; and in that record every member the serving API declares `@sensitive` is
//! PRESENT carrying CloudTrail's fixed redaction literal rather than the value the call
//! carried, while its non-secret siblings are recorded verbatim.
//!
//! Only a cross-service suite can pin this: the events are published by the services that
//! SERVED the calls (Amazon Cognito user pools over awsJson1_1, Amazon SQS over
//! awsJson1_0), the delivery decision belongs to CloudTrail (awsJson1_1), the match and the
//! hand-over belong to EventBridge (awsJson1_1) and what arrived is read back from Amazon
//! SQS — five services over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! which service served the call (a control plane whose records withhold secrets, and one
//! with no sensitive member at all), the event NAME a pattern pins (three writes of ONE
//! service, each reaching only its own rule and never another's), a pattern that names the
//! record's `eventSource` instead of the envelope's `source`, a pattern whose `source`
//! names a DIFFERENT service than the record's `eventName` (so it matches nothing), the
//! OUTCOME of the call (a write the handler refused is published like one that succeeded,
//! carrying the code and message its caller was answered with) and the read/write class the
//! trail's selector admits (a reporting call under a `WriteOnly` selector reaches nothing).
//!
//! The other reader of these very records is `cloudtrail:LookupEvents`, pinned by
//! `core/router/tests/all/cognito_idp_management_events.rs` and, inside the serving crate,
//! by its `tests/all/management_event_recording.rs`; the same channel for a control plane
//! that declares no sensitive member is
//! `core/router/tests/all/cloudtrail_dynamodb_api_call_events.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is served in, and the Region the trail is homed in.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` an event published for an Amazon Cognito user-pool call carries.
const COGNITO_EVENT_SOURCE: &str = "aws.cognito-idp";

/// The `eventSource` the CloudTrail record of an Amazon Cognito user-pool call carries.
const COGNITO_RECORD_SOURCE: &str = "cognito-idp.amazonaws.com";

/// The `source` an event published for an Amazon SQS call carries.
const SQS_EVENT_SOURCE: &str = "aws.sqs";

/// The `eventSource` the CloudTrail record of an Amazon SQS call carries.
const SQS_RECORD_SOURCE: &str = "sqs.amazonaws.com";

/// The literal CloudTrail records in place of a member the serving API declares
/// `@sensitive` (`service_common::management_events::HIDDEN_SENSITIVE_VALUE`).
const REDACTED: &str = "HIDDEN_DUE_TO_SECURITY_REASONS";

/// The access key id every request below presents in its credential scope, which is what a
/// record of the call names its caller by (`userIdentity.accessKeyId`).
const ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// The `userIdentity.type` of a call signed with a long-term access key
/// (`service_common::management_events::PRINCIPAL_TYPE_IAM_USER`).
const PRINCIPAL_TYPE: &str = "IAMUser";

/// The user pool the administrative writes below address.
const POOL_NAME: &str = "api-call-event-pool";

/// The user those writes create, rename nothing of, and remove.
///
/// Distinctive on purpose: the delivered envelopes are scanned for this text, because
/// `AdminCreateUser$Username` is `@sensitive` and no delivered record may carry it.
const USERNAME: &str = "cognito-fan-out-subject-login";

/// The temporary password the create carries: `@sensitive`, and inside
/// `TemporaryPassword`'s modelled `^[\S]+$` pattern and 0-256 length
/// (`smithy_query(query="operation",operation="AdminCreateUser")`). No delivered record may
/// carry it either.
const TEMPORARY_PASSWORD: &str = "Tp1!cognito-fan-out-temporary";

/// The password a later admin set carries — a second `@sensitive` member, on a second
/// operation, so the redaction is pinned as a property of the MEMBER and not of one call.
const PERMANENT_PASSWORD: &str = "Np2!cognito-fan-out-permanent";

/// A user the pool never holds, so the write naming it is REFUSED: CloudTrail records the
/// call and not the answer, so a state-changing call its handler refused is published on
/// this channel too, carrying the outcome it was answered with.
const ABSENT_USERNAME: &str = "cognito-fan-out-absent-login";

/// How long a delivery is waited for. Generous: the suite waits for the CONDITION (the
/// envelope arriving on the queue), and this is only the point at which waiting longer
/// cannot help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How long between two looks at a queue while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The five services this fan-out runs through, over ONE shared engine.
///
/// Amazon S3 is mounted beside them because a trail's destination bucket is resolved in the
/// account's object store: the trail is created the way a client creates one, against a
/// bucket that really exists.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential
    // scope is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and [`REGION`], which is
/// what routes the request and what names the Region it was signed for. The signature is
/// never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon Cognito user-pool call (`awsJson1_1`,
/// `AWSCognitoIdentityProviderService.<Operation>`), as its status and parsed body —
/// whatever it answered.
async fn cognito_answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        "cognito-idp",
        "AWSCognitoIdentityProviderService",
        "1.1",
        operation,
        body,
    )
    .await
}

/// An Amazon Cognito user-pool call (`awsJson1_1`,
/// `AWSCognitoIdentityProviderService.<Operation>`) that must answer 200.
async fn cognito(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = cognito_answered(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon Cognito user pools {operation}, \
         got {status} with {parsed}"
    );
    parsed
}

/// A CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must answer 200.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) =
        rpc(router, "cloudtrail", "CloudTrail_20131101", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
    parsed
}

/// `PUT /<bucket>` plus `PUT /<bucket>?policy` — the bucket the trail delivers its log files
/// into, carrying the bucket policy that lets CloudTrail write into it.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    // Outside the leader Region a create must name the Region it is made in
    // ("IllegalLocationConstraintException" otherwise), which is what this body carries.
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(configuration))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );

    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": {
                "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
            },
        }],
    })
    .to_string();
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}?policy"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(policy))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        (200..300).contains(&response.status().as_u16()),
        "setup: PutBucketPolicy({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// Create a queue and hand back its url and the ARN its OWN service answers for it — the
/// value a client hands EventBridge as a target, taken from the service that owns the
/// queue rather than assembled here. Nothing may send to it yet: the grant is written once
/// the rule that delivers to it exists ([`admitting_rule`]), because it names that rule's
/// ARN.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, name, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {name:?}, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        name,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueArn for {name:?}, got {attributes}"))
        .to_string();
    (url, arn)
}

/// Let `rule_arn` — and only it — send to the queue at `url`: the resource policy a client
/// writes for an EventBridge target, naming the service principal the delivery is made as
/// and scoping it to the rule that makes it.
async fn admitting_rule(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    queue_arn: &str,
    rule_arn: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
}

/// Receive up to ten messages from `url`, draining them, as their parsed bodies — which for
/// a queue target carrying no input transformation is the whole event envelope.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answer["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let body = message["Body"].as_str().unwrap_or_default();
                    serde_json::from_str(body).unwrap_or_else(|error| {
                        panic!("case {case:?}: a delivered envelope is JSON ({error}): {body}")
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look at `url` until it has handed back at least one envelope, or `deadline` has passed —
/// whichever comes first, and then ONCE more. The CONDITION is what is waited for; the
/// deadline only bounds a wait that will never be satisfied, and is passed in so ONE deadline
/// bounds the whole pass rather than one per row.
///
/// The extra look is what lets a row read "exactly one envelope": a SECOND envelope for the
/// same rule arrives a moment after the first, so a drain that stopped at the one it was
/// waiting for could not tell one delivery from two.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    deadline: Instant,
) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if !seen.is_empty() || Instant::now() >= deadline {
            seen.extend(drained(router, case, url).await);
            return seen;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One rule this suite installs: the pattern it matches with, and which call's record — if
/// any — must reach it.
struct Row {
    case: &'static str,
    /// The rule's `EventPattern`.
    pattern: Value,
    /// The envelope `source` and the record `eventName` this row expects, or `None` for a
    /// row that must stay empty.
    expects: Option<(&'static str, &'static str)>,
}

/// The rules, each on its own queue so what arrived at one is never read as another's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the control write of a service whose records withhold nothing",
            pattern: json!({
                "source": [SQS_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [SQS_RECORD_SOURCE],
                    "eventName": ["TagQueue"],
                },
            }),
            expects: Some((SQS_EVENT_SOURCE, "TagQueue")),
        },
        Row {
            case: "the user-pool create, the subject of the rule",
            pattern: json!({
                "source": [COGNITO_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [COGNITO_RECORD_SOURCE],
                    "eventName": ["AdminCreateUser"],
                },
            }),
            expects: Some((COGNITO_EVENT_SOURCE, "AdminCreateUser")),
        },
        Row {
            case: "the near-miss rule of the same service, pinned to the delete",
            pattern: json!({
                "source": [COGNITO_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [COGNITO_RECORD_SOURCE],
                    "eventName": ["AdminDeleteUser"],
                },
            }),
            expects: Some((COGNITO_EVENT_SOURCE, "AdminDeleteUser")),
        },
        Row {
            case: "a rule naming only the record's own eventSource, on a third write",
            pattern: json!({
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [COGNITO_RECORD_SOURCE],
                    "eventName": ["AdminSetUserPassword"],
                },
            }),
            expects: Some((COGNITO_EVENT_SOURCE, "AdminSetUserPassword")),
        },
        Row {
            case: "a state-changing call the handler refused",
            pattern: json!({
                "source": [COGNITO_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [COGNITO_RECORD_SOURCE],
                    "eventName": ["AdminDisableUser"],
                },
            }),
            expects: Some((COGNITO_EVENT_SOURCE, "AdminDisableUser")),
        },
        Row {
            case: "a rule whose source names another service than its eventName",
            pattern: json!({
                "source": [SQS_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": { "eventName": ["AdminCreateUser", "AdminDeleteUser"] },
            }),
            expects: None,
        },
        Row {
            case: "a reporting call of that control plane, outside a WriteOnly selector",
            pattern: json!({
                "source": [COGNITO_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": { "eventName": ["AdminGetUser", "ListUsers"] },
            }),
            expects: None,
        },
    ]
}

/// The `sub` the pool minted for the user, out of the attributes a call answered with: the
/// id every record of a call on that user names it by, since the name the request used is a
/// value the records withhold.
fn answered_sub(case: &str, answered: &Value) -> String {
    answered
        .pointer("/User/Attributes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected the user's attributes: {answered}"))
        .iter()
        .find(|attribute| attribute.get("Name") == Some(&json!("sub")))
        .and_then(|attribute| attribute.get("Value"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the user's sub: {answered}"))
        .to_string()
}

#[tokio::test]
async fn cloudtrail_sourced_api_call_events_reach_the_rules_written_over_a_user_pool() {
    let router = router();

    // A trail of the account, logging the WRITE management events of its own Region.
    create_bucket(&router, "cognito-fan-out-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "cognito-fan-out",
            "S3BucketName": "cognito-fan-out-trail",
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": false,
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "PutEventSelectors",
        &json!({
            "TrailName": "cognito-fan-out",
            "EventSelectors": [{ "ReadWriteType": "WriteOnly", "IncludeManagementEvents": true }],
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "cognito-fan-out" }),
    )
    .await;
    let status = cloudtrail(
        &router,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": "cognito-fan-out" }),
    )
    .await;
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "setup: the trail must be logging, got {status}"
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus — the only bus an
    // AWS-published event reaches. The target carries no input transformation, so what a
    // consumer reads is the whole envelope.
    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("cognito-fan-out-sink-{index}");
        let (url, queue_arn) = create_queue(&router, &name).await;
        let rule = format!("cognito-fan-out-rule-{index}");
        let created = events(
            &router,
            row.case,
            "PutRule",
            &json!({ "Name": rule, "EventPattern": row.pattern.to_string() }),
        )
        .await;
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: expected a RuleArn for {rule:?}, got {created}"))
            .to_string();
        let put = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": rule,
                "Targets": [{ "Id": "sink", "Arn": queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            put["FailedEntryCount"],
            json!(0),
            "case {:?}: the target must be installed, got {put}",
            row.case
        );
        admitting_rule(&router, row.case, &url, &queue_arn, &rule_arn).await;
        queues.push(url);
    }

    // A queue for the control write to act on, so the control rule sees the record of a
    // call the account really served.
    let (control_url, _) = create_queue(&router, "cognito-fan-out-control").await;
    sqs(
        &router,
        "control",
        "TagQueue",
        &json!({ "QueueUrl": control_url, "Tags": { "owner": "api-call-events" } }),
    )
    .await;

    // The calls the rows above are about: the pool, the create carrying two sensitive
    // members, a reporting read of the same user, a second write carrying a third
    // sensitive member, a write the handler refuses, and the delete.
    let pool = cognito(
        &router,
        "setup",
        "CreateUserPool",
        &json!({ "PoolName": POOL_NAME }),
    )
    .await;
    let pool_id = pool["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected the created pool's Id, got {pool}"))
        .to_string();

    let created = cognito(
        &router,
        "the user-pool create",
        "AdminCreateUser",
        &json!({
            "UserPoolId": pool_id,
            "Username": USERNAME,
            "TemporaryPassword": TEMPORARY_PASSWORD,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    assert_eq!(
        created["User"]["UserStatus"],
        json!("FORCE_CHANGE_PASSWORD"),
        "setup: a create carrying a temporary password answers FORCE_CHANGE_PASSWORD, \
         got {created}"
    );
    let sub = answered_sub("the user-pool create", &created);

    cognito(
        &router,
        "the reporting read",
        "AdminGetUser",
        &json!({ "UserPoolId": pool_id, "Username": USERNAME }),
    )
    .await;
    cognito(
        &router,
        "the admin set",
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": USERNAME,
            "Password": PERMANENT_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let (refused_status, refused) = cognito_answered(
        &router,
        "AdminDisableUser",
        &json!({ "UserPoolId": pool_id, "Username": ABSENT_USERNAME }),
    )
    .await;
    assert_eq!(
        refused_status, 400,
        "setup: a write naming a user the pool does not hold must be refused, \
         got {refused_status} with {refused}"
    );
    cognito(
        &router,
        "the delete",
        "AdminDeleteUser",
        &json!({ "UserPoolId": pool_id, "Username": USERNAME }),
    )
    .await;

    // EVERY row expecting a delivery is read first, whatever order `rows()` states them in:
    // once each of them has its envelope the whole fan-out has run, so a row that must stay
    // empty is only then read — after the moment its event would have arrived, rather than
    // before it. The two passes are what makes that independent of the table's order, and ONE
    // deadline bounds the first pass, so a fan-out that never runs fails once rather than per
    // row.
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Vec<Value>> = vec![Vec::new(); rows.len()];
    for (index, (row, url)) in rows.iter().zip(queues.iter()).enumerate() {
        if row.expects.is_some() {
            arrived[index] = delivered(&router, row.case, url, deadline).await;
        }
    }
    for (index, (row, url)) in rows.iter().zip(queues.iter()).enumerate() {
        if row.expects.is_none() {
            arrived[index] = drained(&router, row.case, url).await;
        }
    }

    for ((row, url), delivery) in rows.iter().zip(queues.iter()).zip(arrived.iter()) {
        let case = row.case;
        let Some((source, event_name)) = row.expects else {
            assert!(
                delivery.is_empty(),
                "case {case:?}: expected no delivery on {url}, got {delivery:?}"
            );
            continue;
        };
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered envelope, got {delivery:?}"
        );
        let envelope = &delivery[0];
        assert_eq!(
            envelope["source"],
            json!(source),
            "case {case:?}: expected source {source:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(API_CALL_DETAIL_TYPE),
            "case {case:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["account"],
            json!(ACCOUNT),
            "case {case:?}: expected account {ACCOUNT:?}, got {envelope}"
        );
        assert_eq!(
            envelope["region"],
            json!(REGION),
            "case {case:?}: expected region {REGION:?}, got {envelope}"
        );
        let detail = &envelope["detail"];
        assert_eq!(
            detail["eventName"],
            json!(event_name),
            "case {case:?}: expected detail.eventName {event_name:?}, got {detail}"
        );
        assert_eq!(
            detail["awsRegion"],
            json!(REGION),
            "case {case:?}: expected detail.awsRegion {REGION:?}, got {detail}"
        );
        assert_eq!(
            detail["recipientAccountId"],
            json!(ACCOUNT),
            "case {case:?}: expected detail.recipientAccountId {ACCOUNT:?}, got {detail}"
        );
        assert_eq!(
            detail["readOnly"],
            json!(false),
            "case {case:?}: a write must be recorded readOnly=false, got {detail}"
        );
        assert_eq!(
            detail["eventCategory"],
            json!("Management"),
            "case {case:?}: expected a management record, got {detail}"
        );
        // WHO made the call and WHEN: the key the request presented in its credential
        // scope, the principal class that key belongs to, and the instant the call was
        // served — carried by the record and stamped on the envelope.
        assert_eq!(
            (
                detail.pointer("/userIdentity/type"),
                detail.pointer("/userIdentity/accessKeyId"),
            ),
            (Some(&json!(PRINCIPAL_TYPE)), Some(&json!(ACCESS_KEY_ID))),
            "case {case:?}: expected the record to name the caller that presented \
             {ACCESS_KEY_ID:?}, got {detail}"
        );
        let event_time = detail["eventTime"].as_str().unwrap_or_default();
        assert!(
            event_time.contains('T') && event_time.ends_with('Z'),
            "case {case:?}: expected an instant at detail.eventTime, got {event_time:?}"
        );
        let stamped = envelope["time"].as_str().unwrap_or_default();
        assert!(
            stamped.contains('T') && stamped.ends_with('Z'),
            "case {case:?}: expected the envelope to be stamped with the call's instant, \
             got {stamped:?}"
        );
    }

    // The record of ONE call, found by the event NAME its row pinned rather than by where
    // that row sits in the table: a row inserted, removed or reordered above cannot re-point
    // the assertions below at another call's record.
    let record_of = |event_name: &str| -> Value {
        let found: Vec<&Value> = rows
            .iter()
            .zip(arrived.iter())
            .filter(|(row, _)| row.expects.is_some_and(|(_, name)| name == event_name))
            .flat_map(|(_, delivery)| delivery.iter())
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one delivered record of {event_name:?}, got {found:?}"
        );
        found[0]["detail"].clone()
    };

    // The control write's record: the serving service, and the parameters the call was
    // dispatched with, kept verbatim — this API declares no sensitive member.
    let tagged = record_of("TagQueue");
    assert_eq!(
        tagged["eventSource"],
        json!(SQS_RECORD_SOURCE),
        "the control write's record must name the serving service, got {tagged}"
    );
    assert_eq!(
        tagged["requestParameters"]["queueUrl"],
        json!(control_url),
        "the control write's record must carry the queue it was dispatched with, got {tagged}"
    );

    // The user-pool writes: every sensitive member PRESENT and redacted, every non-secret
    // sibling verbatim, and the user named by the `sub` its own create answered with.
    let create_record = record_of("AdminCreateUser");
    assert_eq!(
        (
            create_record.pointer("/requestParameters/userPoolId"),
            create_record.pointer("/requestParameters/username"),
            create_record.pointer("/requestParameters/temporaryPassword"),
            create_record.pointer("/requestParameters/messageAction"),
        ),
        (
            Some(&json!(pool_id)),
            Some(&json!(REDACTED)),
            Some(&json!(REDACTED)),
            Some(&json!("SUPPRESS")),
        ),
        "the create's record must redact its sensitive members and keep their siblings, \
         got {create_record}"
    );
    assert_eq!(
        (
            create_record.pointer("/responseElements/user/username"),
            create_record.pointer("/responseElements/user/userStatus"),
        ),
        (
            Some(&json!(REDACTED)),
            Some(&json!("FORCE_CHANGE_PASSWORD")),
        ),
        "the create's record must report what it answered, redacted, got {create_record}"
    );
    assert_eq!(
        create_record.pointer("/additionalEventData/sub"),
        Some(&json!(sub)),
        "the create's record must name the user it made by its sub {sub:?}, \
         got {create_record}"
    );

    let set_record = record_of("AdminSetUserPassword");
    assert_eq!(
        (
            set_record.pointer("/requestParameters/userPoolId"),
            set_record.pointer("/requestParameters/username"),
            set_record.pointer("/requestParameters/password"),
            set_record.pointer("/requestParameters/permanent"),
            set_record.pointer("/additionalEventData/sub"),
        ),
        (
            Some(&json!(pool_id)),
            Some(&json!(REDACTED)),
            Some(&json!(REDACTED)),
            Some(&json!(true)),
            Some(&json!(sub)),
        ),
        "the admin set's record must redact its sensitive members and name the same user, \
         got {set_record}"
    );

    let delete_record = record_of("AdminDeleteUser");
    assert_eq!(
        (
            delete_record.pointer("/requestParameters/userPoolId"),
            delete_record.pointer("/requestParameters/username"),
            delete_record.pointer("/additionalEventData/sub"),
        ),
        (
            Some(&json!(pool_id)),
            Some(&json!(REDACTED)),
            Some(&json!(sub)),
        ),
        "the delete's record must redact the name it used and name the same user, \
         got {delete_record}"
    );

    // The OUTCOME of each call: CloudTrail records the call and not the answer, so the three
    // writes that succeeded carry no error at all, while the one the handler refused carries
    // the code and the very message its caller was answered with — and no response
    // elements, because it answered none.
    for record in [&tagged, &create_record, &delete_record, &set_record] {
        assert_eq!(
            (record.get("errorCode"), record.get("errorMessage")),
            (None, None),
            "a call that succeeded records no error, got {record}"
        );
    }
    let refused_record = record_of("AdminDisableUser");
    // The caller's own message, read off the error body it was answered with. Required to be
    // there and to say something: a comparison of two absent values would hold for the wrong
    // reason, and the claim being pinned is that the record carries the very text the SDK
    // raised.
    let answered_message = refused["message"].as_str().unwrap_or_default().to_string();
    assert!(
        !answered_message.is_empty(),
        "the refused write must answer its caller a message to compare the record against, \
         got {refused}"
    );
    assert_eq!(
        (
            refused_record.get("errorCode"),
            refused_record.get("errorMessage"),
            refused_record.pointer("/requestParameters/userPoolId"),
            refused_record.pointer("/requestParameters/username"),
            // Absent or explicitly null both say the same thing — the call answered nothing
            // — so the record is read for what it has rather than for which of the two
            // spellings this emitter uses.
            refused_record
                .get("responseElements")
                .unwrap_or(&Value::Null),
        ),
        (
            Some(&json!("UserNotFoundException")),
            Some(&json!(answered_message)),
            Some(&json!(pool_id)),
            Some(&json!(REDACTED)),
            &Value::Null,
        ),
        "the refused write's record must carry its outcome and the caller's own message \
         {answered_message:?}, got {refused_record}"
    );

    // Nothing anywhere in any delivered envelope carries a value the serving API declares
    // sensitive: the whole point of the redaction is that an event-driven consumer of the
    // audit channel never sees one.
    for (row, delivery) in rows.iter().zip(arrived.iter()) {
        for envelope in delivery {
            let text = envelope.to_string();
            for (secret, what) in [
                (USERNAME, "the user's own name"),
                (ABSENT_USERNAME, "the name the refused write used"),
                (TEMPORARY_PASSWORD, "the temporary password"),
                (PERMANENT_PASSWORD, "the password the admin set"),
            ] {
                assert!(
                    !text.contains(secret),
                    "case {:?}: a delivered envelope must carry no {what}, got {text}",
                    row.case
                );
            }
        }
    }
}
