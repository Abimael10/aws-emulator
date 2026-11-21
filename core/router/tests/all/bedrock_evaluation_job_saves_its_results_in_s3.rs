// SPDX-License-Identifier: CC0-1.0

//! An automated evaluation job Amazon Bedrock reports as finished has SAVED its results in
//! this emulator's own Amazon S3, where a client lists them.
//!
//! RULE: for an evaluation job the result IS the S3 output — `outputDataConfig` is "the
//! Amazon S3 location where the results of your evaluation job are saved" and its `s3Uri` is
//! "the Amazon S3 URI where the results of the evaluation job are saved"
//! (`smithy_query(query="shape",shape="EvaluationOutputDataConfig")`) — so a job that reaches
//! `Completed` has read the records of the dataset its `datasetMetricConfigs` names, invoked
//! the model its `inferenceConfig` names over each of them, scored the metrics that entry
//! configured and written the records under the prefix the submission named: a
//! `ListObjectsV2` over that prefix reports them. A job whose dataset or output location
//! cannot be used reaches `Failed` with `failureMessages` instead and saves nothing.
//!
//! The results are written through the in-process callback into this emulator's own object
//! store, under the service role the submission passed — so a single-service router has no
//! Amazon S3 to write into and no IAM to decide the pass against: that is why this suite
//! lives here rather than in the service's own crate, whose suites pin the halves of the rule
//! that need neither.
//!
//! The rows span what the rule generalises over: the task type and the built-in dataset whose
//! records are evaluated, which metrics the entry configures, the shape of the output location
//! (a prefix, a nested prefix, the bucket itself) and whether the location is one the object
//! store holds at all.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder an awsQuery form body needs is the one
// `all/cloudfront_key_value_store_handshake.rs` already owns: a form component is escaped
// exactly as a URI path segment is, so the codec is shared rather than written again.
use crate::cloudfront_key_value_store_handshake::segment as form_encode;

/// The Region every call here is signed for. It is NOT `us-east-1`, so the bucket is created
/// with the location constraint a bucket outside that Region carries — the shape the recorded
/// case stages its output bucket in.
pub(crate) const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
pub(crate) const ACCOUNT: &str = "000000000000";

/// The bucket the results are saved in, and one no create ever makes.
pub(crate) const BUCKET: &str = "example-evaluation-results";
const ABSENT_BUCKET: &str = "example-evaluation-results-missing";

/// The service role the submission passes, created in IAM below with a trust policy Amazon
/// Bedrock's pass-role step admits.
pub(crate) const ROLE_NAME: &str = "bedrock-evaluation-role";

/// The model the jobs evaluate, named the way the recorded case names it: a
/// `foundation-model` ARN of this Region.
pub(crate) const EVALUATED_MODEL_ARN: &str =
    "arn:aws:bedrock:us-west-2::foundation-model/amazon.nova-lite-v1:0";

/// The model id that ARN resolves to, which is what a result file is named for.
pub(crate) const EVALUATED_MODEL: &str = "amazon.nova-lite-v1:0";

/// The two built-in metrics of an automated job this emulator computes a value for: accuracy,
/// from the response and the record's own reference response, and toxicity, from the response.
pub(crate) const METRIC_ACCURACY: &str =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::evaluation_run::METRIC_ACCURACY;
pub(crate) const METRIC_TOXICITY: &str =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::evaluation_run::METRIC_TOXICITY;

/// How long after the create the job's status is owed its move to the terminal status its run
/// ended in, read off the service's own constant.
pub(crate) const COMPLETE_AFTER: Duration =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_COMPLETE_AFTER;

/// How long a case polls a job before it concludes the lifecycle is stuck: generous next to
/// the span above, room for a slow machine rather than for another order of magnitude.
pub(crate) const POLL_BUDGET: Duration = Duration::from_secs(45);

/// How long a case waits between two reads of a job while it polls it.
pub(crate) const POLL_STEP: Duration = Duration::from_millis(100);

/// The Bedrock control plane, the Bedrock Runtime inference plane, IAM, STS and Amazon S3 over
/// ONE shared engine, as `router/src/lib.rs` mounts them — so the control plane's in-process
/// callback reaches this very object store and this very inference plane, and its pass-role
/// step this very IAM.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        ::service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes the
/// request and what a service reads the caller's Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and key
/// live in the URI and the body is the object's own bytes.
pub(crate) async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/xml")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One awsQuery call to IAM: `POST /` with the parameters in the form body.
pub(crate) async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One restJson1 call to the Bedrock control plane: the status and the answered document.
pub(crate) async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("authorization", authorization("bedrock"))
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `PUT /<bucket>` with the location constraint of a bucket outside `us-east-1`, which is the
/// Region every call here is signed for.
pub(crate) async fn given_bucket(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the output bucket must be created, got {status} with {body:?}"
    );
    // `PutBucketTagging` on the fresh bucket, as a caller labelling its output location does:
    // a tagged bucket is still the bucket the results are written into.
    let tagging = "<Tagging><TagSet><Tag><Key>purpose</Key><Value>evaluation</Value></Tag>\
                   </TagSet></Tagging>";
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?tagging"),
        tagging.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 204,
        "setup: the output bucket must be taggable, got {status} with {body:?}"
    );
}

/// The service role the submissions pass: a role Amazon Bedrock may assume, narrowed to this
/// account as the one it would act for, with a policy granting it the object writes an
/// evaluation job's output needs.
pub(crate) async fn given_role(router: &MultiServiceRouter) -> String {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"bedrock.amazonaws.com\"}},\"Action\":\"sts:AssumeRole\",\
         \"Condition\":{{\"StringEquals\":{{\"aws:SourceAccount\":\"{ACCOUNT}\"}}}}}}]}}"
    );
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the service role must be created, got {status} with {body:?}"
    );
    let permissions = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
         \"Action\":[\"s3:PutObject\",\"s3:GetObject\",\"s3:ListBucket\",\
         \"bedrock:InvokeModel\"],\"Resource\":\"*\"}]}"
        .to_string();
    let (status, body) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "evaluation-output"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the role policy must be written, got {status} with {body:?}"
    );
    // `GetRole`, which is where a client reads the ARN it then passes.
    let (status, document) = iam(router, "GetRole", &[("RoleName", ROLE_NAME)]).await;
    assert_eq!(
        status, 200,
        "setup: the role must be readable, got {status} with {document:?}"
    );
    let arn = document
        .split("<Arn>")
        .nth(1)
        .and_then(|tail| tail.split("</Arn>").next())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        arn,
        format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
        "setup: GetRole reports the role's ARN, got {document:?}"
    );
    arn
}

/// Poll `GetEvaluationJob` the way a client does until the job reports a terminal status, and
/// hand back the document that first reported it.
pub(crate) async fn poll_until_terminal(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> Value {
    let started = ::std::time::Instant::now();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status, document) = bedrock(
            router,
            "GET",
            &format!("/evaluation-jobs/{}", form_encode(arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the job must stay readable while it is polled, got {status} with \
             {document:?}"
        );
        let reported = document
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if ["Completed", "Failed", "Stopped"].contains(&reported.as_str()) {
            return document;
        }
        if seen.last() != Some(&reported) {
            seen.push(reported);
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: a client polling the job must read a terminal status inside \
             {POLL_BUDGET:?} (the run span is {COMPLETE_AFTER:?}), saw {seen:?}"
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

/// Every key the output bucket holds under `prefix`, as `ListObjectsV2` reports them — the
/// call a client makes to find a finished job's results.
pub(crate) async fn listed_keys(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the output bucket must be listable, got {status} with {body:?}"
    );
    body.split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// One row: the job to submit, and what its run must leave behind.
struct Row {
    case: &'static str,
    name: &'static str,
    /// `taskType` — a value of the modelled `EvaluationTaskType`.
    task_type: &'static str,
    /// `dataset.name` — a built-in prompt dataset.
    dataset: &'static str,
    /// `metricNames`.
    metrics: Vec<&'static str>,
    /// `outputDataConfig.s3Uri`, as the submission spells it.
    output_uri: String,
    /// The key prefix the results are listed under, as a reader of the output builds it.
    prefix: &'static str,
    /// Whether the run can be performed at all: a job whose output location the object store
    /// holds nothing of reaches `Failed` and saves nothing.
    runs: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "question answering over the built-in yes-or-no dataset, scored for accuracy, \
                   under a prefix",
            name: "eval-question-answer",
            task_type: "QuestionAndAnswer",
            dataset: "Builtin.BoolQ",
            metrics: vec![METRIC_ACCURACY],
            output_uri: format!("s3://{BUCKET}/eval-output/"),
            prefix: "eval-output",
            runs: true,
        },
        Row {
            case: "generation over the toxicity dataset, scored for toxicity and accuracy, \
                   under a nested prefix",
            name: "eval-generation",
            task_type: "Generation",
            dataset: "Builtin.RealToxicityPrompts",
            metrics: vec![METRIC_TOXICITY, METRIC_ACCURACY],
            output_uri: format!("s3://{BUCKET}/reports/2026/q1"),
            prefix: "reports/2026/q1",
            runs: true,
        },
        Row {
            case: "classification over the built-in review dataset, under the bucket itself",
            name: "eval-classification",
            task_type: "Classification",
            dataset: "Builtin.WomensEcommerceClothingReviews",
            metrics: vec![METRIC_ACCURACY],
            output_uri: format!("s3://{BUCKET}"),
            prefix: "eval-classification",
            runs: true,
        },
        Row {
            case: "an output location in a bucket the object store holds nothing of",
            name: "eval-absent-output",
            task_type: "QuestionAndAnswer",
            dataset: "Builtin.BoolQ",
            metrics: vec![METRIC_ACCURACY],
            output_uri: format!("s3://{ABSENT_BUCKET}/eval-output/"),
            prefix: "eval-output",
            runs: false,
        },
    ]
}

#[tokio::test]
async fn an_evaluation_job_saves_its_results_where_the_submission_named() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;

    for row in rows() {
        let case = row.case;
        let (status, accepted) = bedrock(
            &router,
            "POST",
            "/evaluation-jobs",
            Some(json!({
                "jobName": row.name,
                "roleArn": role_arn,
                "outputDataConfig": {"s3Uri": row.output_uri},
                "evaluationConfig": {"automated": {"datasetMetricConfigs": [{
                    "taskType": row.task_type,
                    "dataset": {"name": row.dataset},
                    "metricNames": row.metrics,
                }]}},
                "inferenceConfig": {"models": [
                    {"bedrockModel": {"modelIdentifier": EVALUATED_MODEL_ARN}},
                ]},
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: the submission must be accepted, got {status} with {accepted:?}"
        );
        let arn = accepted
            .get("jobArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case {case:?}: the submission answers the job's ARN, got {accepted:?}"
        );

        let settled = poll_until_terminal(&router, case, &arn).await;
        let reported = settled.get("status").and_then(Value::as_str).unwrap_or("");
        if !row.runs {
            assert_eq!(
                reported, "Failed",
                "case {case:?}: a job whose results cannot be saved reaches Failed, got {settled:?}"
            );
            assert!(
                settled
                    .get("failureMessages")
                    .and_then(Value::as_array)
                    .is_some_and(|messages| messages
                        .iter()
                        .any(|message| message
                            .as_str()
                            .is_some_and(|message| message.contains(ABSENT_BUCKET)))),
                "case {case:?}: the failure names the location it is about, got {settled:?}"
            );
            continue;
        }

        assert_eq!(
            reported, "Completed",
            "case {case:?}: a job whose run was performed reaches Completed, got {settled:?}"
        );

        // What the client does next: list the prefix it named and read the results out of it.
        let job_id = arn.rsplit('/').next().unwrap_or_default();
        let keys = listed_keys(&router, case, row.prefix).await;
        let expected = {
            let path = format!(
                "{}/{job_id}/models/{EVALUATED_MODEL}/taskTypes/{}/datasets/{}/output.jsonl",
                row.name, row.task_type, row.dataset
            );
            match row.output_uri.trim_end_matches('/').strip_prefix(&format!(
                "s3://{BUCKET}/"
            )) {
                Some(prefix) => format!("{prefix}/{path}"),
                None => path,
            }
        };
        assert!(
            keys.contains(&expected),
            "case {case:?}: a listing of {:?} must report the result object {expected:?}, got \
             {keys:?}",
            row.prefix
        );

        let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{expected}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the result object must be readable, got {status} with {body:?}"
        );
        let records: Vec<Value> = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("case {case:?}: {line:?} is not JSON: {error}"))
            })
            .collect();
        assert!(
            !records.is_empty(),
            "case {case:?}: the result object carries the evaluated records, got {body:?}"
        );
        for (position, record) in records.iter().enumerate() {
            assert!(
                record
                    .pointer("/modelResponses/0/response")
                    .and_then(Value::as_str)
                    .is_some_and(|response| !response.is_empty()),
                "case {case:?}: record {position} carries what the inference answered, got \
                 {record}"
            );
            // Every configured metric is accounted for: a score for the ones computed from
            // this record, and a named entry for the ones no value was computed for.
            let accounted: Vec<&str> = record
                .pointer("/automatedEvaluationResult/scores")
                .and_then(Value::as_array)
                .map(|scores| {
                    scores
                        .iter()
                        .filter_map(|score| score.get("metricName").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            let named: Vec<&str> = record
                .get("unscoredMetrics")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry.get("metricName").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            for metric in &row.metrics {
                assert!(
                    accounted.contains(metric) || named.contains(metric),
                    "case {case:?}: record {position} accounts for the configured metric \
                     {metric:?}, got {record}"
                );
            }
            // Accuracy is computed from the response and the record's own reference response
            // for a question-answering or classification task, so those rows carry a score
            // for it rather than naming it unscored.
            if row.metrics.contains(&METRIC_ACCURACY)
                && ["QuestionAndAnswer", "Classification"].contains(&row.task_type)
            {
                assert!(
                    accounted.contains(&METRIC_ACCURACY),
                    "case {case:?}: record {position} scores {METRIC_ACCURACY} from the \
                     response and its reference response, got {record}"
                );
            }
        }
    }
}
