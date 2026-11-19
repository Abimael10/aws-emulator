// SPDX-License-Identifier: CC0-1.0

//! An endpoint SERVES the model its deployed artifact holds, and CAPTURES what it served where
//! its deployment asks for it.
//!
//! The rules under test:
//!
//! * RULE ONE — `InvokeEndpoint` on an `InService` endpoint answers HTTP 200 with the inference
//!   the model behind the variant computes for the payload, for a model the artifact carries in
//!   the serialized form a boosting container uploads: the prediction is the one the artifact's
//!   OWN parameters imply — the tree a record walks down, through the link its objective
//!   declares — so this suite computes the expected value from the artifact it deployed rather
//!   than from the implementation. Two artifacts answer one payload differently, and the same
//!   pair is answered the same way twice.
//! * RULE TWO — an invocation with no model to compute from is still the modelled 424
//!   `ModelError` naming the condition: an artifact that cannot be read, and one holding no
//!   model's parameters at all.
//! * RULE THREE — an endpoint whose deployment carries a `DataCaptureConfig` writes the
//!   exchange it served under that configuration's `DestinationS3Uri`, as one JSON record per
//!   line, carrying the halves `CaptureOptions` asked for; `EnableCapture` false, a zero
//!   `InitialSamplingPercentage` and a deployment with no capture configuration write nothing.
//! * RULE FOUR — capture is BEST EFFORT: an endpoint whose execution role may not write the
//!   capture destination answers the identical inference with HTTP 200 and leaves no capture
//!   object, so a capture failure is never an inference failure.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — IAM for the role the endpoint reads and
//! writes under, Amazon S3 for the artifact and the capture objects, the SageMaker control
//! plane to deploy, and the SageMaker runtime to invoke. The harness (the emulator, the request
//! builders, the role and the poll-until-settled reading) is the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]) rather than a second copy of it.
//!
//! The rows differ along what the rules generalise over: the model the artifact carries (its
//! split, its leaves and its objective), the payload's media type and shape, the media type the
//! answer is asked for, which halves of the exchange are captured, the sampling share and the
//! form the capture destination's prefix takes.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::CSV_ANSWER_CONTENT_TYPE;
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_object, given_role, iam, image, invoke, send,
    settled, Answer, ACCOUNT, BUCKET, ROLE,
};

/// The variant every endpoint below hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// `CaptureOption$CaptureMode`'s members
/// (`smithy_query(query="shape",shape="CaptureMode")`), as a configuration names them.
const CAPTURE_MODE_INPUT: &str = "Input";
const CAPTURE_MODE_OUTPUT: &str = "Output";
const CAPTURE_MODE_INPUT_AND_OUTPUT: &str = "InputAndOutput";

/// A serialized gradient-boosted model of ONE tree: the root splits feature 0 at `threshold`,
/// a record below it reaching the leaf `low` and one at or above it the leaf `high`, with the
/// learner's objective `objective` and a base score of one half.
///
/// This is the form the boosting container uploads as its model file — the learner's
/// parameters, its objective and the booster's trees as parallel arrays over each tree's nodes
/// — so an endpoint deployed with it is served from the parameters written here.
fn booster(objective: &str, threshold: f64, low: f64, high: f64) -> String {
    json!({
        "learner": {
            "learner_model_param": {
                "base_score": "5E-1",
                "num_feature": "3",
                "num_class": "0",
            },
            "objective": { "name": objective },
            "gradient_booster": {
                "name": "gbtree",
                "model": {
                    "gbtree_model_param": { "num_trees": "1" },
                    "trees": [{
                        "id": 0,
                        "split_indices": [0, 0, 0],
                        "split_conditions": [threshold, low, high],
                        "left_children": [1, -1, -1],
                        "right_children": [2, -1, -1],
                        "base_weights": [0.0, low, high],
                        "default_left": [1, 0, 0],
                    }],
                    "tree_info": [0],
                },
            },
        },
        "version": [2, 0, 0],
    })
    .to_string()
}

/// What the model [`booster`] describes answers for a record: the leaf the record's first
/// feature reaches, added to the base score the margin starts from, through the link the
/// objective declares.
///
/// The expectation is computed from the model's own parameters, so a served answer is compared
/// with the model rather than with another run of the implementation. The base score every
/// document above states is one half, taken back through the link the same way the learner
/// does.
fn expected_prediction(objective: &str, threshold: f64, low: f64, high: f64, first: f64) -> f64 {
    let leaf = if first < threshold { low } else { high };
    match objective {
        // The logit of one half is zero, so the margin is the leaf alone.
        "binary:logistic" | "reg:logistic" => 1.0 / (1.0 + (-leaf).exp()),
        // The logarithm of one half, exponentiated back.
        "count:poisson" => (0.5_f64.ln() + leaf).exp(),
        _ => 0.5 + leaf,
    }
}

/// `PUT /<bucket>/<key>` with raw bytes — `PutObject` for an artifact that is not text.
async fn given_bytes(router: &MultiServiceRouter, key: &str, body: &[u8]) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}/{key}"),
        &[],
        body.to_vec(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: the object {key} must be stored, got {} {}",
        answer.status,
        answer.text()
    );
}

/// The keys Amazon S3 holds under `prefix` — `ListObjectsV2`, read off the listing it answers.
async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let answer = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "the listing of {prefix} must be served, got {} {}",
        answer.status,
        answer.text()
    );
    let listing = answer.text();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next().map(str::to_string))
        .collect()
}

/// The text of one object — `GetObject`.
async fn object_text(router: &MultiServiceRouter, key: &str) -> String {
    let answer = send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new()).await;
    assert_eq!(
        answer.status,
        200,
        "the object {key} must be readable, got {} {}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// Hold a role in this account's IAM that trusts the SageMaker service principal and may READ
/// the bucket but not write it, so a capture write under it is one IAM withholds.
async fn given_read_only_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "Service": service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL,
            },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
            {
                "Effect": "Allow",
                "Action": "s3:ListBucket",
                "Resource": format!("arn:aws:s3:::{BUCKET}"),
            },
        ],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "capture-read-only"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Deploy a model carrying `artifact` behind `endpoint` under `role_arn`, with `capture` as the
/// configuration's `DataCaptureConfig` when the row asks for one, and answer once the endpoint
/// is `InService`.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    endpoint: &str,
    artifact: &str,
    role_arn: &str,
    capture: Option<Value>,
) {
    control_ok(
        router,
        "CreateModel",
        &json!({
            "ModelName": name,
            "PrimaryContainer": { "Image": image(), "ModelDataUrl": artifact },
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    let config = format!("{name}-config");
    let mut body = json!({
        "EndpointConfigName": config,
        "ProductionVariants": [{
            "VariantName": VARIANT,
            "ModelName": name,
            "InstanceType": "ml.m5.large",
            "InitialInstanceCount": 1,
        }],
    });
    if let Some(capture) = capture {
        body["DataCaptureConfig"] = capture;
    }
    control_ok(router, "CreateEndpointConfig", &body).await;
    control_ok(
        router,
        "CreateEndpoint",
        &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
    )
    .await;
    settled(
        router,
        case,
        "DescribeEndpoint",
        &json!({ "EndpointName": endpoint }),
        "EndpointStatus",
        "InService",
    )
    .await;
}

/// The predictions one answer carries, whichever media type it is rendered in.
fn predictions(answer: &Answer, case: &str) -> Vec<f64> {
    if answer.header("content-type").contains("application/json") {
        return answer
            .json()
            .get("predictions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no predictions in {}", answer.text()))
            .iter()
            .map(|entry| {
                entry
                    .get("score")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| panic!("case {case:?}: no score in {entry}"))
            })
            .collect();
    }
    answer
        .text()
        .split(',')
        .map(|field| {
            field
                .trim()
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("case {case:?}: {field:?} is no number: {error}"))
        })
        .collect()
}

/// One row of the model table: a serialized booster and what it answers.
struct ModelRow {
    case: &'static str,
    objective: &'static str,
    threshold: f64,
    low: f64,
    high: f64,
}

/// The model table. Rows differ along the objective the learner declares — which decides the
/// link the margin is answered through — and along the tree's own split and leaves.
fn model_rows() -> Vec<ModelRow> {
    vec![
        ModelRow {
            case: "a binary classifier",
            objective: "binary:logistic",
            threshold: 5.0,
            low: -2.0,
            high: 3.0,
        },
        ModelRow {
            case: "a regression model",
            objective: "reg:squarederror",
            threshold: 50.0,
            low: 1.25,
            high: 7.5,
        },
        ModelRow {
            case: "a count model",
            objective: "count:poisson",
            threshold: 2.0,
            low: -1.0,
            high: 0.5,
        },
    ]
}

/// One row of the payload table: how an invocation is written and what it asks for back.
struct PayloadRow {
    case: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    payload: String,
    /// The first feature of each record the payload carries, which is what the models above
    /// split on.
    first_features: Vec<f64>,
    /// The content type the answer must be served under.
    answer_media_type: &'static str,
}

/// The payload table. Rows differ along the payload's media type and shape, how many records
/// one invocation carries, and the media type the answer is asked for.
fn payload_rows() -> Vec<PayloadRow> {
    vec![
        PayloadRow {
            case: "one comma-separated record, no Accept",
            headers: vec![("content-type", "text/csv")],
            payload: "1,2,3\n".to_string(),
            first_features: vec![1.0],
            answer_media_type: CSV_ANSWER_CONTENT_TYPE,
        },
        PayloadRow {
            case: "three comma-separated records, text/csv asked for",
            headers: vec![("content-type", "text/csv"), ("accept", "text/csv")],
            payload: "1,2,3\n60,0,0\n4.5,1,1\n".to_string(),
            first_features: vec![1.0, 60.0, 4.5],
            answer_media_type: CSV_ANSWER_CONTENT_TYPE,
        },
        PayloadRow {
            case: "a JSON list of records, application/json asked for",
            headers: vec![
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
            payload: json!([[0.0, 0.0, 0.0], [100.0, 1.0, 2.0]]).to_string(),
            first_features: vec![0.0, 100.0],
            answer_media_type: "application/json",
        },
        PayloadRow {
            case: "a JSON instances list, no Accept",
            headers: vec![("content-type", "application/json")],
            payload: json!({ "instances": [[10.0, 20.0, 30.0]] }).to_string(),
            first_features: vec![10.0],
            answer_media_type: CSV_ANSWER_CONTENT_TYPE,
        },
    ]
}

/// RULE ONE: an endpoint answers what the model its artifact holds computes for the payload.
#[tokio::test]
async fn an_endpoint_serves_what_the_model_its_artifact_holds_computes() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;

    // Every row's model is deployed behind its own endpoint, so what differs between the
    // answers below is the model the artifact carries.
    let rows = model_rows();
    for (index, row) in rows.iter().enumerate() {
        let key = format!("artifacts/booster-{index}.json");
        given_object(
            &router,
            &key,
            &booster(row.objective, row.threshold, row.low, row.high),
        )
        .await;
        deploy(
            &router,
            row.case,
            &format!("booster-model-{index}"),
            &format!("booster-endpoint-{index}"),
            &format!("s3://{BUCKET}/{key}"),
            &role_arn,
            None,
        )
        .await;
    }

    let mut per_model: Vec<Vec<f64>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let endpoint = format!("booster-endpoint-{index}");
        let mut answered_here: Vec<f64> = Vec::new();
        for payload in payload_rows() {
            let case = format!("{} / {}", row.case, payload.case);
            let answer = invoke(
                &router,
                &endpoint,
                &payload.headers,
                payload.payload.as_bytes(),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: the invocation must be served, got {} with {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.header("content-type"),
                payload.answer_media_type,
                "case {case:?}: the answer's content type, got {}",
                answer.text()
            );
            assert_eq!(
                answer.header("x-amzn-invoked-production-variant"),
                VARIANT,
                "case {case:?}: the answer must name the variant that served it"
            );
            let served = predictions(&answer, &case);
            let expected: Vec<f64> = payload
                .first_features
                .iter()
                .map(|first| {
                    expected_prediction(row.objective, row.threshold, row.low, row.high, *first)
                })
                .collect();
            assert_eq!(
                served.len(),
                expected.len(),
                "case {case:?}: one prediction per record, got {served:?}"
            );
            for (position, (value, wanted)) in served.iter().zip(&expected).enumerate() {
                assert!(
                    (value - wanted).abs() < 1e-9,
                    "case {case:?}: record {position} must be answered what the artifact's own \
                     model computes, expected {wanted}, got {value}"
                );
            }

            // The answer is COMPUTED: asking again answers the same, so no invocation changed
            // what the next caller is served.
            let again = invoke(
                &router,
                &endpoint,
                &payload.headers,
                payload.payload.as_bytes(),
            )
            .await;
            assert_eq!(
                predictions(&again, &case),
                served,
                "case {case:?}: the same payload against the same artifact must be answered the \
                 same way"
            );
            answered_here.push(served[0]);
        }
        per_model.push(answered_here);
    }

    // The model is the ARTIFACT's: two artifacts answer the same payloads differently.
    for (index, answered) in per_model.iter().enumerate() {
        for (other, compared) in per_model.iter().enumerate().skip(index + 1) {
            assert_ne!(
                answered, compared,
                "case {:?} against {:?}: two artifacts must serve two models, got {answered:?} \
                 and {compared:?}",
                rows[index].case, rows[other].case
            );
        }
    }
}

/// RULE TWO: an invocation with no model to compute from is the modelled 424 `ModelError`
/// naming the condition.
#[tokio::test]
async fn an_invocation_with_no_model_to_compute_from_is_refused_with_model_error() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;

    // An artifact that is GONE by the time the endpoint is invoked, and one that is there and
    // holds no model's parameters at all.
    let deleted_key = "artifacts/deleted.json";
    given_object(&router, deleted_key, &booster("binary:logistic", 5.0, -2.0, 3.0)).await;
    deploy(
        &router,
        "a deleted artifact",
        "deleted-artifact-model",
        "deleted-artifact-endpoint",
        &format!("s3://{BUCKET}/{deleted_key}"),
        &role_arn,
        None,
    )
    .await;
    let deleted = send(
        &router,
        "s3",
        "DELETE",
        &format!("/{BUCKET}/{deleted_key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert!(
        deleted.status == 204 || deleted.status == 200,
        "setup: the artifact must be deleted, got {} {}",
        deleted.status,
        deleted.text()
    );

    let opaque_key = "artifacts/no-model.bin";
    given_bytes(&router, opaque_key, b"\x00\x01\x02 not a serialized model").await;
    deploy(
        &router,
        "an artifact holding no model",
        "no-model-model",
        "no-model-endpoint",
        &format!("s3://{BUCKET}/{opaque_key}"),
        &role_arn,
        None,
    )
    .await;

    // (case, the endpoint, the message the refusal carries)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a deleted artifact",
            "deleted-artifact-endpoint",
            format!(
                "The model artifacts at s3://{BUCKET}/{deleted_key} could not be read, so no \
                 inference was computed."
            ),
        ),
        (
            "an artifact holding no model",
            "no-model-endpoint",
            format!(
                "The model artifacts at s3://{BUCKET}/{opaque_key} hold no model this emulator \
                 can compute an inference from: a SageMaker model runs as a container image, \
                 which this emulator does not run, so the artifact has to carry the model's own \
                 parameters."
            ),
        ),
    ];
    for (case, endpoint, expected) in rows {
        let answer = invoke(&router, endpoint, &[("content-type", "text/csv")], b"1,2,3").await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (424, "ModelError"),
            "case {case:?}: got {} with {}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: the refusal must name the condition that stopped the inference"
        );
    }
}

/// One row of the capture table: the configuration the deployment carries and what the
/// destination must hold after one served invocation.
struct CaptureRow {
    case: &'static str,
    /// The `DataCaptureConfig` the endpoint configuration carries, when it carries one.
    capture: Option<Value>,
    /// The prefix the destination names, under the bucket.
    prefix: &'static str,
    /// Whether the invocation's request half must be in the record.
    expect_input: bool,
    /// Whether the invocation's answer half must be in the record.
    expect_output: bool,
    /// Whether any capture object must be written at all.
    expect_object: bool,
}

/// One `DataCaptureConfig` body.
fn capture_config(prefix: &str, sampling: i32, enabled: Option<bool>, modes: &[&str]) -> Value {
    let mut config = json!({
        "InitialSamplingPercentage": sampling,
        "DestinationS3Uri": format!("s3://{BUCKET}/{prefix}"),
        "CaptureOptions": modes
            .iter()
            .map(|mode| json!({ "CaptureMode": mode }))
            .collect::<Vec<Value>>(),
    });
    if let Some(enabled) = enabled {
        config["EnableCapture"] = json!(enabled);
    }
    config
}

/// The capture table. Rows differ along which halves of the exchange are captured, whether
/// capture is enabled, the sampling share, the form the destination's prefix takes, and a
/// deployment carrying no capture configuration at all.
fn capture_rows() -> Vec<CaptureRow> {
    vec![
        CaptureRow {
            case: "both halves, a prefix ending in a separator",
            capture: Some(capture_config(
                "capture-both/",
                100,
                Some(true),
                &[CAPTURE_MODE_INPUT, CAPTURE_MODE_OUTPUT],
            )),
            prefix: "capture-both/",
            expect_input: true,
            expect_output: true,
            expect_object: true,
        },
        CaptureRow {
            case: "the InputAndOutput mode, a nested prefix with no separator",
            capture: Some(capture_config(
                "nested/capture-io",
                100,
                None,
                &[CAPTURE_MODE_INPUT_AND_OUTPUT],
            )),
            prefix: "nested/capture-io",
            expect_input: true,
            expect_output: true,
            expect_object: true,
        },
        CaptureRow {
            case: "the request alone",
            capture: Some(capture_config(
                "capture-input/",
                100,
                Some(true),
                &[CAPTURE_MODE_INPUT],
            )),
            prefix: "capture-input/",
            expect_input: true,
            expect_output: false,
            expect_object: true,
        },
        CaptureRow {
            case: "the answer alone",
            capture: Some(capture_config(
                "capture-output/",
                100,
                Some(true),
                &[CAPTURE_MODE_OUTPUT],
            )),
            prefix: "capture-output/",
            expect_input: false,
            expect_output: true,
            expect_object: true,
        },
        CaptureRow {
            case: "capture turned off",
            capture: Some(capture_config(
                "capture-off/",
                100,
                Some(false),
                &[CAPTURE_MODE_INPUT, CAPTURE_MODE_OUTPUT],
            )),
            prefix: "capture-off/",
            expect_input: false,
            expect_output: false,
            expect_object: false,
        },
        CaptureRow {
            case: "no share of the requests at all",
            capture: Some(capture_config(
                "capture-none/",
                0,
                Some(true),
                &[CAPTURE_MODE_INPUT, CAPTURE_MODE_OUTPUT],
            )),
            prefix: "capture-none/",
            expect_input: false,
            expect_output: false,
            expect_object: false,
        },
        CaptureRow {
            case: "a deployment carrying no capture configuration",
            capture: None,
            prefix: "capture-absent/",
            expect_input: false,
            expect_output: false,
            expect_object: false,
        },
    ]
}

/// The record one capture object holds, read as the JSON line it is.
fn capture_record(text: &str, case: &str) -> Value {
    let line = text
        .lines()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the capture object holds no record"));
    serde_json::from_str(line)
        .unwrap_or_else(|error| panic!("case {case:?}: {line:?} is no JSON record: {error}"))
}

/// RULE THREE: a served invocation is captured where the deployment asks for it, and nowhere
/// else.
#[tokio::test]
async fn a_served_invocation_is_captured_as_the_deployment_asks() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;
    let artifact_key = "artifacts/capture-booster.json";
    given_object(
        &router,
        artifact_key,
        &booster("binary:logistic", 5.0, -2.0, 3.0),
    )
    .await;
    let artifact = format!("s3://{BUCKET}/{artifact_key}");

    // The invocation every row is driven with, and the id it is captured under.
    let payload = "7,8,9\n";
    let inference_id = "capture-inference-1";
    let content_type = "text/csv";

    for (index, row) in capture_rows().iter().enumerate() {
        let case = row.case;
        let endpoint = format!("capture-endpoint-{index}");
        deploy(
            &router,
            case,
            &format!("capture-model-{index}"),
            &endpoint,
            &artifact,
            &role_arn,
            row.capture.clone(),
        )
        .await;
        let answer = invoke(
            &router,
            &endpoint,
            &[
                ("content-type", content_type),
                ("x-amzn-sagemaker-inference-id", inference_id),
            ],
            payload.as_bytes(),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answer.status,
            answer.text()
        );

        // The capture write is part of serving the invocation, so what the destination holds is
        // settled by the time the answer came back.
        let keys = keys_under(&router, row.prefix).await;
        if !row.expect_object {
            assert!(
                keys.is_empty(),
                "case {case:?}: nothing must be captured, got {keys:?}"
            );
            continue;
        }
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: one served invocation is one capture record, got {keys:?}"
        );
        let key = &keys[0];
        assert!(
            key.starts_with(&format!(
                "{}/{endpoint}/{VARIANT}/",
                row.prefix.trim_end_matches('/')
            )) && key.ends_with(".jsonl"),
            "case {case:?}: the capture object is laid out under the endpoint and the variant, \
             got {key:?}"
        );
        let text = object_text(&router, key).await;
        assert!(
            text.ends_with('\n'),
            "case {case:?}: a capture object holds one record per line, got {text:?}"
        );
        let record = capture_record(&text, case);
        assert_eq!(
            record.get("eventVersion").and_then(Value::as_str),
            Some("0"),
            "case {case:?}: the record declares its schema version, got {record}"
        );
        assert_eq!(
            record
                .pointer("/eventMetadata/inferenceId")
                .and_then(Value::as_str),
            Some(inference_id),
            "case {case:?}: the request's InferenceId is added to the captured data, got {record}"
        );
        let event_id = record
            .pointer("/eventMetadata/eventId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !event_id.is_empty() && key.contains(event_id),
            "case {case:?}: the record names the event its object is keyed by, got {record}"
        );
        assert!(
            record
                .pointer("/eventMetadata/inferenceTime")
                .and_then(Value::as_str)
                .is_some_and(|at| at.ends_with('Z')),
            "case {case:?}: the record reports when the invocation was served, got {record}"
        );

        let captured_input = record.pointer("/captureData/endpointInput");
        let captured_output = record.pointer("/captureData/endpointOutput");
        assert_eq!(
            captured_input.is_some(),
            row.expect_input,
            "case {case:?}: CaptureOptions decides whether the request is recorded, got {record}"
        );
        assert_eq!(
            captured_output.is_some(),
            row.expect_output,
            "case {case:?}: CaptureOptions decides whether the answer is recorded, got {record}"
        );
        if let Some(half) = captured_input {
            assert_eq!(
                (
                    half.get("mode").and_then(Value::as_str),
                    half.get("data").and_then(Value::as_str),
                    half.get("observedContentType").and_then(Value::as_str),
                    half.get("encoding").and_then(Value::as_str),
                ),
                (Some("INPUT"), Some(payload), Some(content_type), Some("CSV")),
                "case {case:?}: the request half carries the payload as it was sent, got {half}"
            );
        }
        if let Some(half) = captured_output {
            assert_eq!(
                (
                    half.get("mode").and_then(Value::as_str),
                    half.get("data").and_then(Value::as_str),
                    half.get("encoding").and_then(Value::as_str),
                ),
                (Some("OUTPUT"), Some(answer.text().as_str()), Some("CSV")),
                "case {case:?}: the answer half carries the inference as it was served, got {half}"
            );
        }
    }
}

/// One exchange a capture row drives: the case, the request headers and the payload.
type ExchangeCase = (&'static str, Vec<(&'static str, &'static str)>, String);

/// RULE THREE, continued: a JSON exchange is captured as the JSON it was, and every served
/// invocation adds a record of its own.
#[tokio::test]
async fn each_served_invocation_adds_its_own_capture_record() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;
    let artifact_key = "artifacts/json-capture-booster.json";
    given_object(
        &router,
        artifact_key,
        &booster("reg:squarederror", 5.0, -2.0, 3.0),
    )
    .await;
    let prefix = "capture-json/";
    deploy(
        &router,
        "a JSON exchange",
        "json-capture-model",
        "json-capture-endpoint",
        &format!("s3://{BUCKET}/{artifact_key}"),
        &role_arn,
        Some(capture_config(
            prefix,
            100,
            Some(true),
            &[CAPTURE_MODE_INPUT, CAPTURE_MODE_OUTPUT],
        )),
    )
    .await;

    // Rows: the same endpoint invoked with payloads of different shapes, so each invocation is
    // a record of its own and the records differ along the exchange they captured.
    let rows: Vec<ExchangeCase> = vec![
        (
            "a JSON payload answered as JSON",
            vec![
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
            json!([[1.0, 2.0, 3.0]]).to_string(),
        ),
        (
            "a JSON payload answered as CSV",
            vec![("content-type", "application/json")],
            json!({ "instances": [[3.0, 4.0, 5.0], [50.0, 6.0, 7.0]] }).to_string(),
        ),
        (
            "a comma-separated payload",
            vec![("content-type", "text/csv")],
            "7,8,9\n".to_string(),
        ),
    ];
    for (index, (case, headers, payload)) in rows.iter().enumerate() {
        let answer = invoke(&router, "json-capture-endpoint", headers, payload.as_bytes()).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answer.status,
            answer.text()
        );
        let keys = keys_under(&router, prefix).await;
        assert_eq!(
            keys.len(),
            index + 1,
            "case {case:?}: every served invocation adds a capture record, got {keys:?}"
        );
        // The record this invocation added is the one no earlier invocation could have: it
        // carries this payload, recorded as the form its content type declares.
        let mut carried = Vec::new();
        for key in &keys {
            let text = object_text(&router, key).await;
            let record = capture_record(&text, case);
            carried.push(
                record
                    .pointer("/captureData/endpointInput/data")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
            let observed = record
                .pointer("/captureData/endpointInput/observedContentType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let encoding = record
                .pointer("/captureData/endpointInput/encoding")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let expected = if observed.contains("json") { "JSON" } else { "CSV" };
            assert_eq!(
                encoding, expected,
                "case {case:?}: a {observed} payload is recorded as the text it was, got {record}"
            );
        }
        assert!(
            carried.iter().any(|data| data == payload),
            "case {case:?}: the destination must hold this invocation's own payload, got \
             {carried:?}"
        );
    }
}

/// RULE FOUR: capture is best effort — a destination the execution role may not write changes
/// nothing about the inference.
#[tokio::test]
async fn a_capture_write_the_role_may_not_make_leaves_the_inference_unchanged() {
    let router = emulator();
    given_bucket(&router).await;
    // One artifact, two endpoints: one whose role may write the capture destination and one
    // whose role may only read. The model is the artifact's, so both must answer identically.
    let writing_role = given_role(&router).await;
    let reading_role = given_read_only_role(&router, "sagemaker-capture-read-only").await;
    let artifact_key = "artifacts/best-effort-booster.json";
    let objective = "binary:logistic";
    let (threshold, low, high) = (5.0, -2.0, 3.0);
    given_object(&router, artifact_key, &booster(objective, threshold, low, high)).await;
    let artifact = format!("s3://{BUCKET}/{artifact_key}");

    // (case, the model's execution role, the endpoint, the destination prefix, whether the
    // capture object must be there)
    let rows: Vec<(&str, &str, &str, &str, bool)> = vec![
        (
            "a role granted s3:PutObject on the destination",
            &writing_role,
            "best-effort-allowed",
            "best-effort-allowed/",
            true,
        ),
        (
            "a role withheld s3:PutObject on the destination",
            &reading_role,
            "best-effort-denied",
            "best-effort-denied/",
            false,
        ),
    ];
    let payload = "11,12,13\n";
    let mut answered: Vec<String> = Vec::new();
    for (case, role_arn, endpoint, prefix, captured) in rows {
        deploy(
            &router,
            case,
            &format!("{endpoint}-model"),
            endpoint,
            &artifact,
            role_arn,
            Some(capture_config(
                prefix,
                100,
                Some(true),
                &[CAPTURE_MODE_INPUT, CAPTURE_MODE_OUTPUT],
            )),
        )
        .await;
        let answer = invoke(
            &router,
            endpoint,
            &[("content-type", "text/csv")],
            payload.as_bytes(),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the inference is answered whatever capture could do, got {} with {}",
            answer.status,
            answer.text()
        );
        let keys = keys_under(&router, prefix).await;
        assert_eq!(
            !keys.is_empty(),
            captured,
            "case {case:?}: expected captured={captured}, got {keys:?}"
        );
        answered.push(answer.text());
    }
    let expected = expected_prediction(objective, threshold, low, high, 11.0);
    for (index, served) in answered.iter().enumerate() {
        let read = served
            .parse::<f64>()
            .unwrap_or_else(|error| panic!("the answer must be a number: {served:?} ({error})"));
        assert!(
            (read - expected).abs() < 1e-9,
            "case {index}: the artifact's own model answers {expected}, got {read}"
        );
    }
    assert_eq!(
        answered[0], answered[1],
        "case \"capture is best effort\": the same artifact answers the same payload the same \
         way whether or not the exchange could be captured, got {answered:?}"
    );
    // The role that may not write the capture destination is a real IAM record of this
    // account's, not an absent one.
    assert!(
        reading_role.ends_with(":role/sagemaker-capture-read-only") && !reading_role.contains(ROLE),
        "the read-only role must be its own IAM role, got {reading_role:?}"
    );
}
