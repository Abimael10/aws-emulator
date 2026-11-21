// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a call it will not serve AT ALL — the two faults that fail the
//! WHOLE request rather than one item of it.
//!
//! A value outside the modelled `SageMakerResourceArn` `@pattern` fails the whole call
//! with `400 ValidationException` and ONE constant sentence — no member path, no
//! collection position, no submitted value and no expression quoted back — whatever the
//! offending value is, wherever in `MetricQueries` it sits and however many queries the
//! batch carries.
//!
//! IAM's authorization decision is the other: a caller whose identity-based policies do not
//! allow `sagemaker:<OperationName>` on a resource the request addresses is refused
//! `403 AccessDeniedException` with IAM's own sentence, before either operation reads or
//! writes a metric.
//!
//! Every other modelled constraint of either operation is judged by the operation itself,
//! per item, inside a 200 response — and a reference the expression ACCEPTS but that no
//! resource answers stays a per-query `Status=ValidationError` inside a 200, so the two
//! channels remain distinguishable.
//!
//! Every test drives the real wire path (`MultiServiceRouter::dispatch` with a restJson1
//! request), so the framework's constraint walk and this service's refusal seam are both
//! exercised.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::ops::batch_get_metrics::{
    INVALID_RESOURCE_ARN_CODE, INVALID_RESOURCE_ARN_MESSAGE, SAGEMAKER_RESOURCE_ARN_PATTERN,
};
use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// An epoch second the metric points in these tests are written at.
const T0: i64 = 1_700_000_000;

/// How the framework opens its own per-member constraint clause: the envelope every
/// refusal this service does NOT word itself keeps.
const FRAMEWORK_ENVELOPE_OPENING: &str = "validation error";

/// The access-key id a call presents unless a case varies it: neither STS nor IAM holds a
/// record for it, so it resolves to the account's own ambient identity, which holds no IAM
/// policies — the position AWS account root credentials are in.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The Region a call is signed for unless a case varies it.
const REGION: &str = "us-east-1";

/// One emulator with this service mounted on a single shared engine, exactly as the
/// router mounts it in production.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the framework's business and are not what these tests pin.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What one call answered: the status, the error code (the restJson1 `__type` and the
/// `x-amzn-errortype` header, which must agree) and the parsed body.
struct Answer {
    status: u16,
    error_type: Option<String>,
    header_error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// The `message` member of an error body, or the empty string for a success body.
    fn message(&self) -> &str {
        self.body.get("message").and_then(Value::as_str).unwrap_or("")
    }
}

/// Dispatch one restJson1 request.
/// Dispatch one restJson1 request, as the account's own published credential.
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: &Value) -> Answer {
    call_as(router, ACCOUNT_KEY, REGION, method, path, body).await
}

/// Dispatch one restJson1 request signed for `region` with `access_key_id` in its SigV4
/// credential scope — which is where the caller's identity and the Region of the resource
/// ARNs a decision is taken against are read from.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    region: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://metrics.sagemaker.{region}.amazonaws.com{path}"))
        .header("host", format!("metrics.sagemaker.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        // The SigV4 credential scope is how the router picks the service; the signature
        // itself is not verified here (authentication is off above).
        .header(
            "authorization",
            authorization_header(access_key_id, region, "sagemaker"),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: response body is not JSON ({e}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    Answer {
        status,
        error_type: parsed
            .get("__type")
            .and_then(Value::as_str)
            .map(str::to_string),
        header_error_type,
        body: parsed,
    }
}

/// One `MetricQuery` over `resource_arn`, every other member well inside its constraints.
fn query(resource_arn: &str) -> Value {
    json!({
        "MetricName": "loss",
        "ResourceArn": resource_arn,
        "MetricStat": "Last",
        "Period": "IterationNumber",
        "XAxisType": "IterationNumber",
    })
}

/// `POST /BatchGetMetrics` over `queries`.
async fn get_metrics(router: &MultiServiceRouter, queries: Vec<Value>) -> Answer {
    call(
        router,
        "POST",
        "/BatchGetMetrics",
        &json!({ "MetricQueries": queries }),
    )
    .await
}

/// Assert `answer` is the refusal a malformed SageMaker resource reference gets: the
/// whole call refused with the one constant sentence and nothing of the request in it.
fn assert_refused_as_invalid_arn(case: &str, answer: &Answer, submitted: &[&str]) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected status 400, got {} ({:?})",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some(INVALID_RESOURCE_ARN_CODE),
        "case {case:?}: expected __type {INVALID_RESOURCE_ARN_CODE}, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.header_error_type.as_deref(),
        Some(INVALID_RESOURCE_ARN_CODE),
        "case {case:?}: expected x-amzn-errortype {INVALID_RESOURCE_ARN_CODE}, got {:?}",
        answer.header_error_type
    );
    assert_eq!(
        answer.message(),
        INVALID_RESOURCE_ARN_MESSAGE,
        "case {case:?}: expected message {INVALID_RESOURCE_ARN_MESSAGE:?}, got {:?}",
        answer.message()
    );
    // The sentence is ALL fixed text: nothing of the request, of the member path or of
    // the modelled expression is interpolated into it.
    for value in submitted.iter().filter(|value| !value.is_empty()) {
        assert!(
            !answer.message().contains(value),
            "case {case:?}: the message must not quote the submitted value {value:?}, \
             got {:?}",
            answer.message()
        );
    }
    for leak in [
        "metricQueries",
        "resourceArn",
        FRAMEWORK_ENVELOPE_OPENING,
        SAGEMAKER_RESOURCE_ARN_PATTERN,
    ] {
        assert!(
            !answer.message().contains(leak),
            "case {case:?}: the message must not carry {leak:?}, got {:?}",
            answer.message()
        );
    }
}

/// Values the modelled `SageMakerResourceArn` `@pattern`
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:[a-z\-].*/`, from
/// `smithy_query(query="shape",shape="SageMakerResourceArn")`) REJECTS, one per way of
/// being malformed. Every one is inside the member's `@length 0..2048`, so the pattern is
/// the only constraint any of them breaks.
fn malformed_arns() -> Vec<(&'static str, String)> {
    vec![
        ("not-an-arn-at-all", "not-an-arn".to_string()),
        (
            "no-resource-separator",
            "arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component".to_string(),
        ),
        (
            "another-services-arn",
            "arn:aws:s3:::my-bucket/metrics".to_string(),
        ),
        (
            "account-not-twelve-digits",
            "arn:aws:sagemaker:us-east-1:12345:experiment-trial-component/tc".to_string(),
        ),
        (
            "resource-type-not-lowercase",
            "arn:aws:sagemaker:us-east-1:123456789012:Experiment-Trial-Component/tc".to_string(),
        ),
        (
            "uppercase-arn-scheme",
            "ARN:AWS:SAGEMAKER:us-east-1:123456789012:experiment-trial-component/tc".to_string(),
        ),
        (
            "no-resource-type",
            "arn:aws:sagemaker:us-east-1:123456789012:/tc".to_string(),
        ),
        (
            "partition-separator-missing",
            "arn-aws-sagemaker-us-east-1-123456789012-experiment-trial-component/tc".to_string(),
        ),
        // Long, but well inside @length max 2048: length is not what refuses it.
        ("long-but-not-an-arn", "x".repeat(2_000)),
    ]
}

/// Values the same expression ACCEPTS: the refusal keys on the expression, so these are
/// served rather than refused — across partitions, regions, accounts and resource types.
fn well_formed_arns() -> Vec<(&'static str, String)> {
    vec![
        (
            "trial-component",
            "arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/tc-arn-rules"
                .to_string(),
        ),
        (
            "china-partition-training-job",
            "arn:aws-cn:sagemaker:cn-north-1:210987654321:training-job/job-7".to_string(),
        ),
        (
            "govcloud-partition-processing-job",
            "arn:aws-us-gov:sagemaker:us-gov-west-1:111122223333:processing-job/proc".to_string(),
        ),
        (
            // `[a-z0-9\-]*` admits an empty region, as a global resource has.
            "empty-region",
            "arn:aws:sagemaker::123456789012:experiment-trial-component/global".to_string(),
        ),
        (
            // `.*` admits further path segments after the resource type.
            "nested-resource-path",
            "arn:aws:sagemaker:eu-west-1:123456789012:pipeline/my-pipeline/execution/abc"
                .to_string(),
        ),
    ]
}

/// Every way of being malformed, at every position of batches of several sizes: one
/// constant sentence each.
#[tokio::test]
async fn a_malformed_resource_arn_refuses_the_whole_call_with_one_constant_sentence() {
    let router = emulator();
    let good = well_formed_arns()[0].1.clone();

    for (shape, malformed) in malformed_arns() {
        // The same value alone, first, last and in the middle of a longer batch, and
        // twice in one batch: the answer does not vary with position or count.
        let layouts: Vec<(&str, Vec<Value>)> = vec![
            ("alone", vec![query(&malformed)]),
            ("first-of-two", vec![query(&malformed), query(&good)]),
            ("last-of-two", vec![query(&good), query(&malformed)]),
            (
                "middle-of-three",
                vec![query(&good), query(&malformed), query(&good)],
            ),
            (
                "twice-in-four",
                vec![
                    query(&good),
                    query(&malformed),
                    query(&good),
                    query(&malformed),
                ],
            ),
        ];
        for (layout, queries) in layouts {
            let case = format!("{shape}/{layout}");
            let answer = get_metrics(&router, queries).await;
            assert_refused_as_invalid_arn(&case, &answer, &[&malformed, &good]);
        }
    }
}

/// A reference the expression accepts is SERVED: the refusal is about the expression, not
/// about the one recorded resource.
#[tokio::test]
async fn a_well_formed_resource_arn_is_served() {
    let router = emulator();
    for (case, arn) in well_formed_arns() {
        let answer = get_metrics(&router, vec![query(&arn)]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a well-formed reference is served, got {} ({:?})",
            answer.status, answer.body
        );
        let results = answer.body["MetricQueryResults"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected MetricQueryResults, got {:?}", answer.body));
        assert_eq!(
            results.len(),
            1,
            "case {case:?}: expected one result per query, got {results:?}"
        );
        assert_eq!(
            results[0]["Status"].as_str(),
            Some("Complete"),
            "case {case:?}: nothing was written for this resource, which is Complete and \
             empty, got {:?}",
            results[0]
        );
    }
}

/// The refusal is about a reference the request CARRIES: a query that states no reference
/// at all states nothing malformed, so it is answered in-band like any query that selects
/// nothing.
#[tokio::test]
async fn a_reference_the_request_does_not_state_is_not_a_malformed_one() {
    let router = emulator();
    // case, the query as sent.
    let cases: Vec<(&str, Value)> = vec![
        ("empty-reference", query("")),
        (
            "null-reference",
            json!({
                "MetricName": "loss",
                "ResourceArn": Value::Null,
                "MetricStat": "Last",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }),
        ),
        (
            "no-reference-member",
            json!({
                "MetricName": "loss",
                "MetricStat": "Last",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }),
        ),
    ];
    for (case, sent) in cases {
        let answer = get_metrics(&router, vec![sent]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a query stating no reference is served, got {} ({:?})",
            answer.status, answer.body
        );
        let results = answer.body["MetricQueryResults"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected MetricQueryResults, got {:?}", answer.body));
        assert_eq!(
            results[0]["Status"].as_str(),
            Some("Complete"),
            "case {case:?}: it names no series, which is Complete and empty, got {:?}",
            results[0]
        );
        assert_eq!(
            results[0]["MetricValues"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: it selected nothing, got {:?}",
            results[0]
        );
    }
}

/// The refusal is about the SageMaker-ARN expression alone: every other constraint class
/// — including another member's own `@pattern` — is answered per item inside a 200, or by
/// this service's own envelope refusal, never with this sentence.
#[tokio::test]
async fn other_constraint_violations_are_not_this_refusal() {
    let router = emulator();
    let good = well_formed_arns()[0].1.clone();

    // case, method, path, body, the status the service answers it at — one per constraint
    // class the model declares elsewhere. Each is answered by the layer that owns it: the
    // envelope refusal for a call that carries nothing to work on, a 200 with the per-item
    // verdict for everything else — and never with this refusal's sentence.
    let cases: Vec<(&str, &str, &str, Value, u16)> = vec![
        (
            // MetricQueryList is modelled @length 1..100; a get with no queries is this
            // service's own envelope refusal.
            "empty-metric-queries",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [] }),
            500,
        ),
        (
            // MetricStatistic enumerates no `Median`: the query names no statistic this
            // service can read, so it selects nothing and is answered in-band.
            "metric-stat-outside-the-enum",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": "loss",
                "ResourceArn": good,
                "MetricStat": "Median",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }] }),
            200,
        ),
        (
            // MetricName is modelled @length 1..255 @pattern '^.+$': another member's
            // pattern, which this service does NOT refuse the call for.
            "metric-name-empty",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": "",
                "ResourceArn": good,
                "MetricStat": "Last",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }] }),
            200,
        ),
        (
            // ExperimentEntityName is modelled @pattern '^[a-z0-9](-*[a-z0-9]){0,119}$'.
            "trial-component-name-uppercase",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "TC-Arn-Rules",
                "MetricData": [{
                    "MetricName": "loss",
                    "Timestamp": T0,
                    "Step": 0,
                    "Value": 1.0,
                }],
            }),
            200,
        ),
        (
            // Step is modelled @range min 0.
            "negative-step",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "tc-arn-rules",
                "MetricData": [{
                    "MetricName": "loss",
                    "Timestamp": T0,
                    "Step": -1,
                    "Value": 1.0,
                }],
            }),
            200,
        ),
    ];

    for (case, method, path, body, expected) in cases {
        let answer = call(&router, method, path, &body).await;
        assert_eq!(
            answer.status, expected,
            "case {case:?}: expected status {expected}, got {} ({:?})",
            answer.status, answer.body
        );
        assert_ne!(
            answer.message(),
            INVALID_RESOURCE_ARN_MESSAGE,
            "case {case:?}: this is not a resource-reference fault, got {:?}",
            answer.body
        );
        // What it IS answered with: the per-item verdict for a served call, and a refusal
        // worded by the layer that owns it — neither this one nor the framework's
        // per-member constraint envelope — for a refused one.
        if expected == 200 {
            assert!(
                answer.body.get("MetricQueryResults").is_some()
                    || answer.body.get("Errors").is_some(),
                "case {case:?}: a served call reports its items, got {:?}",
                answer.body
            );
        } else {
            assert_ne!(
                answer.error_type.as_deref(),
                Some(INVALID_RESOURCE_ARN_CODE),
                "case {case:?}: this refusal is not worded as the resource-reference one, \
                 got {:?}",
                answer.body
            );
            assert!(
                !answer.message().contains(FRAMEWORK_ENVELOPE_OPENING),
                "case {case:?}: the framework's per-member envelope must not reach the \
                 caller, got {:?}",
                answer.body
            );
        }
    }
}

/// A request that breaks the ARN expression AND another constraint is answered as the
/// resource-reference fault: the reference is resolved before anything else is read.
#[tokio::test]
async fn a_malformed_arn_beside_another_violation_is_still_the_arn_fault() {
    let router = emulator();
    let answer = get_metrics(
        &router,
        vec![json!({
            "MetricName": "",
            "ResourceArn": "not-an-arn",
            "MetricStat": "Median",
            "Period": "IterationNumber",
            "XAxisType": "IterationNumber",
        })],
    )
    .await;
    assert_refused_as_invalid_arn("arn-and-enum-and-name", &answer, &["not-an-arn", "Median"]);
}

/// The two channels stay distinct: a reference the expression ACCEPTS is answered in-band,
/// per query, inside a 200 — and a sibling query is still answered from what
/// `BatchPutMetrics` stored.
#[tokio::test]
async fn a_reference_the_expression_accepts_is_answered_in_band() {
    let router = emulator();
    let component = "tc-arn-channels";
    let put = call(
        &router,
        "PUT",
        "/BatchPutMetrics",
        &json!({
            "TrialComponentName": component,
            "MetricData": [{
                "MetricName": "loss",
                "Timestamp": T0,
                "Step": 0,
                "Value": 4.0,
            }],
        }),
    )
    .await;
    assert_eq!(
        put.status, 200,
        "BatchPutMetrics must answer 200, got {} ({:?})",
        put.status, put.body
    );
    assert!(
        put.body
            .get("Errors")
            .is_none_or(|errors| errors.as_array().is_none_or(Vec::is_empty)),
        "the batch must be accepted, got {:?}",
        put.body
    );

    let stored = format!("arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/{component}");
    // The expression only requires the resource part to CONTAIN a `/`, so this passes it
    // and still names no resource.
    let no_name_segment = "arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/";
    let answer = get_metrics(&router, vec![query(no_name_segment), query(&stored)]).await;
    assert_eq!(
        answer.status, 200,
        "a reference the expression accepts is answered in-band, got {} ({:?})",
        answer.status, answer.body
    );
    let results = answer.body["MetricQueryResults"]
        .as_array()
        .unwrap_or_else(|| panic!("expected MetricQueryResults, got {:?}", answer.body))
        .clone();
    assert_eq!(
        results.len(),
        2,
        "one result per query, positionally aligned, got {results:?}"
    );
    assert_eq!(
        results[0]["Status"].as_str(),
        Some("Complete"),
        "case {no_name_segment:?}: a reference the expression accepts selects nothing, \
         which is Complete, got {:?}",
        results[0]
    );
    assert_eq!(
        results[0]["MetricValues"].as_array().map(Vec::len),
        Some(0),
        "case {no_name_segment:?}: it selected nothing, got {:?}",
        results[0]
    );
    assert_eq!(
        results[1]["Status"].as_str(),
        Some("Complete"),
        "case {stored:?}: the sibling query is answered from state, got {:?}",
        results[1]
    );
    assert_eq!(
        results[1]["MetricValues"].as_array().map(Vec::len),
        Some(1),
        "case {stored:?}: expected the one stored point, got {:?}",
        results[1]
    );
    let value = results[1]["MetricValues"][0].as_f64();
    assert_eq!(
        value,
        Some(4.0),
        "case {stored:?}: expected the stored value 4.0, got {value:?}"
    );
}

// The second fault that refuses the whole call: IAM's authorization decision on the CALLER.

/// A second Region, for the cases that vary which Region a call was signed for.
const OTHER_REGION: &str = "eu-west-3";

/// The namespace SageMaker publishes this service's actions under, which is also its SigV4
/// signing name and the namespace of the resource ARNs a decision is taken against.
const ACTION_PREFIX: &str = "sagemaker";

/// The code and the status an authorization refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The `RoleSessionName` every `AssumeRole` below asks for — the last segment of the
/// assumed-role ARN a refusal names as the principal.
const SESSION: &str = "probe";

/// A SigV4 `Authorization` header whose credential scope names `access_key_id`, `region` and
/// `signing_name`. Request authentication is off in these tests, so the signature is not
/// checked; the scope is what the caller's identity and the ARN Region are read from.
fn authorization_header(access_key_id: &str, region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20261029/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One emulator with this service mounted beside the two services a CALLER's own identity is
/// made with — IAM, which holds the principals and the documents in force on them, and STS,
/// which mints the sessions — all on ONE shared engine, as the assembled emulator mounts
/// them. So the policies the gate reads are the ones those services wrote.
fn emulator_with_identity_services() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the framework's business and are not what these tests pin.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the round
/// trip.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One awsQuery call to IAM or STS, made with the account's own credential, that must be
/// served.
async fn aws_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            authorization_header(ACCOUNT_KEY, REGION, signing_name),
        )
        .body(bytes::Bytes::from(body))
        .expect("build the awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup {action}: the call must succeed, got {status} with {text}"
    );
    text
}

/// A caller: the access key id its requests present, and the principal a refusal names.
struct Principal {
    access_key_id: String,
    arn: String,
}

/// Create an IAM role carrying `policies` as its inline policies, assume it through STS, and
/// answer the session's access key id with the session ARN.
async fn assumed_role(
    router: &MultiServiceRouter,
    role_name: &str,
    policies: &[String],
) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    aws_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    for (index, policy) in policies.iter().enumerate() {
        aws_query(
            router,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", format!("inline-{index}")),
                ("PolicyDocument", policy.clone()),
            ],
        )
        .await;
    }
    let assumed = aws_query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", format!("arn:aws:iam::{account}:role/{role_name}")),
            ("RoleSessionName", SESSION.to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: xml_element(&assumed, "AccessKeyId")
            .unwrap_or_else(|| panic!("AssumeRole answers an AccessKeyId, got {assumed}")),
        arn: xml_element(&assumed, "Arn")
            .unwrap_or_else(|| panic!("AssumeRole answers the session's Arn, got {assumed}")),
    }
}

/// One identity-based policy document carrying one statement.
fn statement(effect: &str, actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": actions, "Resource": resource}],
    })
    .to_string()
}

/// The ARN of a trial component of THIS account, spelled here independently of the crate
/// under test — as SageMaker's Service Authorization Reference spells the
/// `experiment-trial-component` resource type.
fn component_arn(region: &str, component: &str) -> String {
    format!(
        "arn:aws:{ACTION_PREFIX}:{region}:{}:experiment-trial-component/{component}",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with, stated here rather than read off the crate under
/// test: who was refused, which action, on which resource, and which of IAM's rules refused.
fn denial(principal: &str, operation: &str, resource: &str, explicit_deny: bool) -> String {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let reason = if explicit_deny {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action")
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// One operation of this service as a CALLER reaches it: the name that is also the last
/// segment of the IAM action, the method and path an SDK addresses it at, and the body it
/// carries for the trial component under test, in the Region whose ARNs it names.
struct Call {
    operation: &'static str,
    method: &'static str,
    path: &'static str,
    body: fn(&str, &str) -> Value,
}

const CALLS: &[Call] = &[
    Call {
        operation: "BatchPutMetrics",
        method: "PUT",
        path: "/BatchPutMetrics",
        // The ingest names its trial component by NAME; the Region of the resource the
        // decision is taken against is the one the caller SIGNED for.
        body: |component, _region| {
            json!({
                "TrialComponentName": component,
                "MetricData": [{
                    "MetricName": "loss",
                    "Timestamp": T0,
                    "Step": 0,
                    "Value": 7.5,
                }],
            })
        },
    },
    Call {
        operation: "BatchGetMetrics",
        method: "POST",
        path: "/BatchGetMetrics",
        // The read names its resource by ARN, one per query.
        body: |component, region| json!({ "MetricQueries": [query(&component_arn(region, component))] }),
    },
];

/// One row of the grant table: the documents the caller's role carries — built from the
/// action the call asks for and the ARN of the trial component it addresses — and whether IAM
/// allows the call under them.
struct Grant {
    case: &'static str,
    policies: fn(&str, &str) -> Vec<String>,
    allowed: bool,
    explicit_deny: bool,
}

const GRANTS: &[Grant] = &[
    Grant {
        case: "a role carrying no policy at all",
        policies: |_action, _arn| Vec::new(),
        allowed: false,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted every SageMaker action",
        policies: |_action, _arn| vec![statement("Allow", &["sagemaker:*"], "*")],
        allowed: true,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted this call's action on this trial component",
        policies: |action, arn| vec![statement("Allow", &[action], arn)],
        allowed: true,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted this call's action on every trial component of the account",
        policies: |action, _arn| vec![statement("Allow", &[action], &component_arn(REGION, "*"))],
        allowed: true,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted every action on ANOTHER trial component",
        policies: |_action, _arn| {
            vec![statement(
                "Allow",
                &["sagemaker:*"],
                &component_arn(REGION, "another-component"),
            )]
        },
        allowed: false,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted another service's actions",
        policies: |_action, _arn| vec![statement("Allow", &["s3:*"], "*")],
        allowed: false,
        explicit_deny: false,
    },
    Grant {
        case: "a role granted every action with this call's action denied",
        policies: |action, _arn| {
            vec![
                statement("Allow", &["sagemaker:*"], "*"),
                statement("Deny", &[action], "*"),
            ]
        },
        allowed: false,
        explicit_deny: true,
    },
];

/// Assert `answer` is the authorization refusal `principal` is owed for `operation` on
/// `resource`.
fn assert_denied(
    case: &str,
    answer: &Answer,
    principal: &str,
    operation: &str,
    resource: &str,
    explicit_deny: bool,
) {
    assert_eq!(
        answer.status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {} ({:?})",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected __type {ACCESS_DENIED}, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.header_error_type.as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected x-amzn-errortype {ACCESS_DENIED}, got {:?}",
        answer.header_error_type
    );
    assert_eq!(
        answer.message(),
        denial(principal, operation, resource, explicit_deny),
        "case {case:?}: expected IAM's own sentence, got {:?}",
        answer.message()
    );
}

/// How many points `component` holds for the metric these calls write, read back with the
/// account's own credential — so a refused ingest can be shown to have stored nothing.
async fn stored_points(router: &MultiServiceRouter, component: &str) -> usize {
    let answer = get_metrics(router, vec![query(&component_arn(REGION, component))]).await;
    assert_eq!(
        answer.status, 200,
        "reading {component} back must answer 200, got {} ({:?})",
        answer.status, answer.body
    );
    let results = answer.body["MetricQueryResults"]
        .as_array()
        .unwrap_or_else(|| panic!("expected MetricQueryResults, got {:?}", answer.body))
        .clone();
    assert_eq!(
        results[0]["Status"].as_str(),
        Some("Complete"),
        "reading {component} back is Complete whether or not anything was stored, got {:?}",
        results[0]
    );
    results[0]["MetricValues"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("expected MetricValues, got {:?}", results[0]))
}

/// RULE: both operations are decided against the CALLING principal's own identity-based
/// policies, under `sagemaker:<OperationName>` on the trial component the request addresses —
/// so a caller no readable statement grants, or one an explicit deny refuses, is answered
/// `403 AccessDeniedException` with IAM's own sentence and ingests nothing, while the same
/// call is served once the grant is in force.
#[tokio::test]
async fn both_operations_are_decided_against_the_callers_identity_policies() {
    let router = emulator_with_identity_services();
    for (call_index, call) in CALLS.iter().enumerate() {
        let action = format!("{ACTION_PREFIX}:{}", call.operation);
        for (grant_index, grant) in GRANTS.iter().enumerate() {
            let component = format!("tc-authz-{call_index}-{grant_index}");
            let resource = component_arn(REGION, &component);
            let case = format!("{} / {}", call.operation, grant.case);
            let caller = assumed_role(
                &router,
                &format!("smm-authz-{call_index}-{grant_index}"),
                &(grant.policies)(&action, &resource),
            )
            .await;
            let answer = call_as(
                &router,
                &caller.access_key_id,
                REGION,
                call.method,
                call.path,
                &(call.body)(&component, REGION),
            )
            .await;
            if grant.allowed {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: a permitted caller must be served, got {} ({:?})",
                    answer.status, answer.body
                );
            } else {
                assert_denied(
                    &case,
                    &answer,
                    &caller.arn,
                    call.operation,
                    &resource,
                    grant.explicit_deny,
                );
            }
            // Whatever the verdict, only a PERMITTED ingest leaves a point behind: a refused
            // call performs nothing at all.
            let expected = usize::from(grant.allowed && call.operation == "BatchPutMetrics");
            let held = stored_points(&router, &component).await;
            assert_eq!(
                held, expected,
                "case {case:?}: expected {expected} stored point(s) for {component}, got {held}"
            );
        }
    }
}

/// RULE: the account's own published credential holds no IAM policies — the position AWS
/// account root credentials are in — so it is served, and what it writes is queryable.
#[tokio::test]
async fn the_accounts_own_credential_is_served() {
    let router = emulator_with_identity_services();
    let component = "tc-authz-account";
    for call in CALLS {
        let answer = call_as(
            &router,
            ACCOUNT_KEY,
            REGION,
            call.method,
            call.path,
            &(call.body)(component, REGION),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {}: the account's own credential must be served, got {} ({:?})",
            call.operation, answer.status, answer.body
        );
    }
    let held = stored_points(&router, component).await;
    assert_eq!(
        held, 1,
        "the account's own ingest must be queryable afterwards, got {held} point(s)"
    );
}

/// RULE: the resource a decision is taken against comes from the REQUEST — the trial
/// component an ingest names, in the Region it was signed for, and the reference a query
/// carries for a read. So a grant scoped to one Region's component ARN allows the ingest
/// signed for that Region and refuses the identical one signed for another, naming the ARN of
/// the Region it was signed for; a read is decided on the reference it carries whatever Region
/// it was signed for.
#[tokio::test]
async fn the_resource_comes_from_the_request_and_the_region_it_was_signed_for() {
    let router = emulator_with_identity_services();
    let component = "tc-authz-region";
    let resource = component_arn(REGION, component);
    let caller = assumed_role(
        &router,
        "smm-authz-region",
        &[statement(
            "Allow",
            &[
                &format!("{ACTION_PREFIX}:BatchPutMetrics"),
                &format!("{ACTION_PREFIX}:BatchGetMetrics"),
            ],
            &resource,
        )],
    )
    .await;

    // (case, the call, the Region signed for, the status expected, the resource a refusal
    // must name)
    let ingest = &CALLS[0];
    let read = &CALLS[1];
    let rows: Vec<(&str, &Call, &str, u16, String)> = vec![
        (
            "the ingest signed for the granted Region",
            ingest,
            REGION,
            200,
            resource.clone(),
        ),
        (
            "the same ingest signed for another Region",
            ingest,
            OTHER_REGION,
            ACCESS_DENIED_STATUS,
            component_arn(OTHER_REGION, component),
        ),
        (
            "the read of the granted reference, signed for another Region",
            read,
            OTHER_REGION,
            200,
            resource.clone(),
        ),
    ];
    for (case, call, region, expected_status, named) in rows {
        // The body always names the GRANTED reference; what varies is the credential scope.
        let answer = call_as(
            &router,
            &caller.access_key_id,
            region,
            call.method,
            call.path,
            &(call.body)(component, REGION),
        )
        .await;
        if expected_status == 200 {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the call to be served, got {} ({:?})",
                answer.status, answer.body
            );
        } else {
            assert_denied(case, &answer, &caller.arn, call.operation, &named, false);
        }
    }
}

/// RULE: a read is decided against EVERY reference its queries carry, so a caller granted one
/// of two referenced components is refused the batch naming both — the refusal naming the
/// component it holds no grant for — and the same batch is served once both are granted.
#[tokio::test]
async fn a_read_is_decided_against_every_reference_its_queries_carry() {
    let router = emulator_with_identity_services();
    let granted = component_arn(REGION, "tc-authz-granted");
    let ungranted = component_arn(REGION, "tc-authz-ungranted");
    let action = format!("{ACTION_PREFIX}:BatchGetMetrics");
    let batch = vec![query(&granted), query(&ungranted)];

    let partial = assumed_role(
        &router,
        "smm-authz-partial",
        &[statement("Allow", &[action.as_str()], &granted)],
    )
    .await;
    let refused = call_as(
        &router,
        &partial.access_key_id,
        REGION,
        "POST",
        "/BatchGetMetrics",
        &json!({ "MetricQueries": batch.clone() }),
    )
    .await;
    assert_denied(
        "a batch naming one granted and one ungranted component",
        &refused,
        &partial.arn,
        "BatchGetMetrics",
        &ungranted,
        false,
    );
    assert!(
        refused.body.get("MetricQueryResults").is_none(),
        "a refused read must answer no query at all, got {:?}",
        refused.body
    );

    let both = assumed_role(
        &router,
        "smm-authz-both",
        &[statement(
            "Allow",
            &[action.as_str()],
            &component_arn(REGION, "*"),
        )],
    )
    .await;
    let served = call_as(
        &router,
        &both.access_key_id,
        REGION,
        "POST",
        "/BatchGetMetrics",
        &json!({ "MetricQueries": batch }),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "a caller granted both references must be served, got {} ({:?})",
        served.status, served.body
    );
    assert_eq!(
        served.body["MetricQueryResults"].as_array().map(Vec::len),
        Some(2),
        "one result per query, got {:?}",
        served.body
    );
}
