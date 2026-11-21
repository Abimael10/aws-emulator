// SPDX-License-Identifier: CC0-1.0

//! The results of an accepted evaluation job reach Amazon S3 on a router that VERIFIES
//! request signatures — the configuration the emulator serves traffic in.
//!
//! RULE: for an evaluation job the result IS the S3 output — `outputDataConfig` is "the
//! Amazon S3 location where the results of your evaluation job are saved"
//! (`smithy_query(query="shape",shape="EvaluationOutputDataConfig")`) — and the run that
//! writes it is this service's own in-process call into this emulator's Amazon S3. That hop
//! is signed with the emulator's own credentials and is verified exactly as a client's own
//! request is, so a job reported in a terminal status on an AUTHENTICATING router has its
//! results under its output prefix, as it does on one that verifies nothing.
//!
//! That is the half the sibling suites cannot pin: `all/bedrock_evaluation_job_*.rs` drive a
//! router with verification OFF, where a hop whose signature the object store would refuse
//! still stores its object. Here the whole flow — `CreateBucket`, the service role, the
//! submission, the lifecycle read and the listing of the output — is signed the way an SDK
//! signs it, and the job's own `failureMessages` are asserted empty, so a run the object
//! store refused is reported as such rather than hidden behind an empty prefix.
//!
//! The rows differ along what the rule generalises over: the task type and built-in dataset
//! whose records are evaluated, and the shape of the output location (a prefix, the bucket
//! itself).

use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_key_value_store_handshake::segment as form_encode;

/// The Region every call here is signed for and addressed in.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The bucket the results are saved in.
const BUCKET: &str = "example-signed-evaluation-results";

/// The service role the submission passes.
const ROLE_NAME: &str = "bedrock-signed-evaluation-role";

/// The model the jobs evaluate, by the `foundation-model` ARN spelling, and the model id it
/// resolves to — which is what a result file is named for.
const EVALUATED_MODEL_ARN: &str =
    "arn:aws:bedrock:us-west-2::foundation-model/amazon.nova-lite-v1:0";
const EVALUATED_MODEL: &str = "amazon.nova-lite-v1:0";

/// The built-in metric the rows configure.
const METRIC_ACCURACY: &str =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::evaluation_run::METRIC_ACCURACY;

/// How long after the create the job's status is owed its move to the terminal status its run
/// ended in, read off the service's own constant.
const COMPLETE_AFTER: Duration =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_COMPLETE_AFTER;

/// How long a case polls before it concludes the lifecycle is stuck: generous next to the span
/// above, room for a slow machine rather than for another order of magnitude.
const POLL_BUDGET: Duration = Duration::from_secs(45);

/// How long a case waits between two polls.
const POLL_STEP: Duration = Duration::from_millis(100);

/// The XML namespace a `CreateBucketConfiguration` carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The emulator as it is ASSEMBLED, on one engine, with request authentication ON: the
/// control plane's in-process calls into Amazon S3, IAM and the inference plane are dispatched
/// back into this same router and verified there.
fn emulator() -> MultiServiceRouter {
    let engine = ::aws_emulator_router::new_engine();
    let mut router = ::aws_emulator_router::build_router(engine);
    router.set_request_authentication(true);
    router
}

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
fn published() -> &'static (String, String) {
    static PUBLISHED: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PUBLISHED.get_or_init(|| {
        let minted = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        (
            minted.access_key_id.clone(),
            minted.secret_access_key.clone(),
        )
    })
}

/// `request`, signed for `signing_name` in [`REGION`] the way an SDK signs one.
fn signed(mut request: http::Request<Vec<u8>>, signing_name: &str) -> http::Request<Bytes> {
    let (key_id, secret) = published();
    let identity = Credentials::new(key_id, secret, None, None, "test").into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(signing_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    let (parts, payload) = request.into_parts();
    http::Request::from_parts(parts, Bytes::from(payload))
}

/// One signed request against S3's REST surface, path-style: the status and the body text.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .body(body.to_vec())
        .expect("a well-formed request");
    let response = router.dispatch(signed(request, "s3")).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One signed awsQuery call to IAM.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = "iam.amazonaws.com".to_string();
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.into_bytes())
        .expect("a well-formed request");
    let response = router.dispatch(signed(request, "iam")).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One signed restJson1 call to the Bedrock control plane: the status and the document.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("bedrock.{REGION}.amazonaws.com");
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("the test body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(signed(request, "bedrock")).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The output bucket, created in the Region every call here addresses.
async fn given_bucket(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
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
}

/// The service role the submissions pass: one Amazon Bedrock may assume, with a policy
/// granting it the object writes an evaluation job's output needs.
async fn given_role(router: &MultiServiceRouter) -> String {
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
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}")
}

/// Every key the output bucket holds under `prefix`, as `ListObjectsV2` reports them.
async fn listed_keys(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
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

/// Poll `GetEvaluationJob` until the job reports a terminal status, and hand back the document
/// that first reported it.
async fn poll_until_terminal(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let started = std::time::Instant::now();
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

/// One row: the job to submit, and where its results are to be found.
struct Row {
    case: &'static str,
    name: &'static str,
    task_type: &'static str,
    dataset: &'static str,
    output_uri: String,
    prefix: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "question answering over the built-in yes-or-no dataset, under a prefix",
            name: "eval-signed-question-answer",
            task_type: "QuestionAndAnswer",
            dataset: "Builtin.BoolQ",
            output_uri: format!("s3://{BUCKET}/eval-output/"),
            prefix: "eval-output",
        },
        Row {
            case: "classification over the built-in review dataset, under the bucket itself",
            name: "eval-signed-classification",
            task_type: "Classification",
            dataset: "Builtin.WomensEcommerceClothingReviews",
            output_uri: format!("s3://{BUCKET}"),
            prefix: "eval-signed-classification",
        },
    ]
}

impl Row {
    /// The result object this row's run saves.
    fn expected_key(&self, job_id: &str) -> String {
        let path = format!(
            "{}/{job_id}/models/{EVALUATED_MODEL}/taskTypes/{}/datasets/{}/output.jsonl",
            self.name, self.task_type, self.dataset
        );
        match self
            .output_uri
            .trim_end_matches('/')
            .strip_prefix(&format!("s3://{BUCKET}/"))
        {
            Some(prefix) => format!("{prefix}/{path}"),
            None => path,
        }
    }
}

#[tokio::test]
async fn an_evaluation_jobs_results_reach_s3_through_the_signed_in_process_hop() {
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
                    "metricNames": [METRIC_ACCURACY],
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
        let job_id = arn.rsplit('/').next().unwrap_or_default().to_string();

        // The job's own verdict first: a run the object store refused is reported as the
        // modelled `Failed` with the sentences it failed on, so this assertion names them
        // rather than leaving an empty prefix to explain.
        let settled = poll_until_terminal(&router, case, &arn).await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: the run must be performed through the signed hop, got {settled:?}"
        );

        let key = row.expected_key(&job_id);
        let keys = listed_keys(&router, case, row.prefix).await;
        assert!(
            keys.contains(&key),
            "case {case:?}: a listing of {:?} must report the result object {key:?}, got \
             {keys:?}",
            row.prefix
        );
        let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the result object must be readable, got {status} with {body:?}"
        );
        assert!(
            body.lines()
                .filter(|line| !line.trim().is_empty())
                .all(|line| serde_json::from_str::<Value>(line)
                    .is_ok_and(|record| record
                        .pointer("/modelResponses/0/response")
                        .and_then(Value::as_str)
                        .is_some_and(|response| !response.is_empty()))),
            "case {case:?}: every saved record carries what the inference answered, got \
             {body:?}"
        );
    }
}
