// SPDX-License-Identifier: CC0-1.0

//! What an `InService` endpoint's answer DECLARES about itself: the production variant that
//! served the invocation, and the content type the inference bytes carry.
//!
//! RULE: an invocation of an endpoint whose `EndpointStatus` is `InService` is answered HTTP 200
//! with the inference the variant it routed to computed; the answer names that variant in
//! `x-Amzn-Invoked-Production-Variant` — the variant the ENDPOINT CONFIGURATION listed, or the
//! one `X-Amzn-SageMaker-Target-Variant` selected — and declares in `Content-Type` the content
//! type those bytes really carry ("the MIME type of the inference returned from the model
//! container", `InvokeEndpointOutput$ContentType`): a textual rendering names the character set
//! it is written in, and a media type whose own definition fixes the encoding takes no such
//! parameter. The modelled `ModelError` (424) answers an invocation the model did not serve, so
//! it is never how an invocation of an otherwise healthy `InService` endpoint closes.
//!
//! Only a cross-service suite can ask this: the flow spans the services `core/router/src/lib.rs`
//! mounts on ONE engine — IAM for the execution role, Amazon S3 for the deployed artifact, the
//! SageMaker control plane to deploy, and the SageMaker runtime to invoke. The emulator, the
//! request builders, the role and the poll-until-settled reading are the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]) rather than a second copy of them; what
//! is written here is the deployment a row needs, which lists SEVERAL variants, one model behind
//! each.
//!
//! The rows differ along what the rule generalises over: how many variants the endpoint
//! configuration lists and what they are NAMED, whether the request selects one of them, the
//! form the payload is written in, how many records it carries, and the media type the answer is
//! asked for.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{CSV_ANSWER_CONTENT_TYPE, JSON_MEDIA_TYPE};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_object, given_role, image, invoke, predictions,
    settled, BUCKET,
};

/// The coefficients the deployed artifact's model carries, one per feature, and the bias it adds.
/// Every expectation below is computed from these rather than from another run of the
/// implementation.
const COEFFICIENTS: [f64; 3] = [2.0, -1.5, 0.25];
const BIAS: f64 = 4.0;

/// A serialized LINEAR booster carrying [`COEFFICIENTS`] and [`BIAS`] — the form a boosting
/// container uploads as its model file: the learner's parameters, its objective, and the
/// booster's weights, one per feature and then the bias.
fn artifact_document() -> String {
    let mut weights: Vec<f64> = COEFFICIENTS.to_vec();
    weights.push(BIAS);
    json!({
        "learner": {
            "learner_model_param": {
                "base_score": "0",
                "num_feature": COEFFICIENTS.len().to_string(),
                "num_class": "0",
            },
            "objective": { "name": "reg:squarederror" },
            "gradient_booster": { "name": "gblinear", "model": { "weights": weights } },
        },
        "version": [2, 0, 0],
    })
    .to_string()
}

/// What the model that artifact holds answers for one record: its coefficients against the
/// record plus its bias, through the identity link its objective declares.
fn expected_prediction(record: &[f64]) -> f64 {
    BIAS + COEFFICIENTS
        .iter()
        .zip(record)
        .map(|(weight, value)| weight * value)
        .sum::<f64>()
}

/// How far a served prediction may sit from the value the artifact's own parameters imply: the
/// two are the same arithmetic, so this leaves room for nothing but the order the sum is
/// accumulated in.
const PREDICTION_TOLERANCE: f64 = 1e-9;

/// One row: the variants the endpoint hosts, what the request selects and asks for, and what the
/// answer must declare.
struct Row {
    case: &'static str,
    /// The variant names the endpoint configuration lists, in that order.
    variants: Vec<&'static str>,
    /// `X-Amzn-SageMaker-Target-Variant`, when the request selects a variant itself.
    target: Option<&'static str>,
    /// The request's `Accept`, when it asks for a media type.
    accept: Option<&'static str>,
    /// The records the payload carries.
    records: Vec<Vec<f64>>,
    /// Whether the payload is written as JSON rather than as comma-separated records.
    json_payload: bool,
    /// The variant the answer must name.
    invoked: &'static str,
    /// The content type the answer must be served under.
    content_type: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one variant, no Accept",
            variants: vec!["AllTraffic"],
            target: None,
            accept: None,
            records: vec![vec![1.0, 2.0, 3.0]],
            json_payload: false,
            invoked: "AllTraffic",
            content_type: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "a variant the configuration names otherwise, text/csv asked for",
            variants: vec!["primary-shard-1"],
            target: None,
            accept: Some("text/csv"),
            records: vec![vec![4.0, 0.5, -2.0], vec![0.0, 0.0, 0.0]],
            json_payload: false,
            invoked: "primary-shard-1",
            content_type: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "two variants, the request selects the second",
            variants: vec!["blue", "green"],
            target: Some("green"),
            accept: Some("text/csv"),
            records: vec![vec![10.0, 20.0, 30.0]],
            json_payload: false,
            invoked: "green",
            content_type: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "two variants, the request selects none",
            variants: vec!["blue", "green"],
            target: None,
            accept: None,
            records: vec![vec![10.0, 20.0, 30.0], vec![1.5, 2.5, 3.5]],
            json_payload: false,
            invoked: "blue",
            content_type: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "application/json asked for",
            variants: vec!["json-variant"],
            target: Some("json-variant"),
            accept: Some("application/json"),
            records: vec![vec![2.0, 4.0, 8.0], vec![-1.0, -2.0, -3.0]],
            json_payload: false,
            invoked: "json-variant",
            content_type: JSON_MEDIA_TYPE,
        },
        Row {
            case: "a JSON payload, no Accept",
            variants: vec!["all-traffic-2"],
            target: None,
            accept: None,
            records: vec![vec![3.0, 6.0, 9.0]],
            json_payload: true,
            invoked: "all-traffic-2",
            content_type: CSV_ANSWER_CONTENT_TYPE,
        },
    ]
}

/// One row's payload: a JSON list of records, or one comma-separated record per line.
fn payload(records: &[Vec<f64>], json_payload: bool) -> String {
    if json_payload {
        return json!(records).to_string();
    }
    records
        .iter()
        .map(|record| {
            let values: Vec<String> = record.iter().map(f64::to_string).collect();
            format!("{}\n", values.join(","))
        })
        .collect()
}

/// Deploy the artifact at `artifact` behind `endpoint`, on one variant per entry of `variants`
/// with a model of its own, and answer once the endpoint reports `InService`.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    endpoint: &str,
    variants: &[&str],
    artifact: &str,
    role_arn: &str,
) {
    let mut production: Vec<Value> = Vec::new();
    for (index, variant) in variants.iter().enumerate() {
        let model = format!("{name}-model-{index}");
        control_ok(
            router,
            "CreateModel",
            &json!({
                "ModelName": model,
                "PrimaryContainer": { "Image": image(), "ModelDataUrl": artifact },
                "ExecutionRoleArn": role_arn,
            }),
        )
        .await;
        production.push(json!({
            "VariantName": variant,
            "ModelName": model,
            "InstanceType": "ml.m5.large",
            "InitialInstanceCount": 1,
        }));
    }
    let config = format!("{name}-config");
    control_ok(
        router,
        "CreateEndpointConfig",
        &json!({ "EndpointConfigName": config, "ProductionVariants": production }),
    )
    .await;
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

/// The rule: a served invocation closes 200, names the variant that served it, and declares the
/// content type its bytes carry.
#[tokio::test]
async fn a_served_invocation_names_its_variant_and_declares_its_content_type() {
    let router = emulator();
    given_bucket(&router).await;
    // One execution role serves every row: what the rows vary is the deployment's variants and
    // what the request asks for, not who the endpoint reads its artifact as.
    let role_arn = given_role(&router).await;
    let key = "artifacts/answer-booster.json";
    given_object(&router, key, &artifact_document()).await;
    let artifact = format!("s3://{BUCKET}/{key}");

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let endpoint = format!("answer-endpoint-{index}");
        deployed(
            &router,
            case,
            &format!("answer-{index}"),
            &endpoint,
            &row.variants,
            &artifact,
            &role_arn,
        )
        .await;

        let body = payload(&row.records, row.json_payload);
        let mut headers: Vec<(&str, &str)> = vec![(
            "content-type",
            if row.json_payload {
                "application/json"
            } else {
                "text/csv"
            },
        )];
        if let Some(accept) = row.accept {
            headers.push(("accept", accept));
        }
        if let Some(target) = row.target {
            headers.push(("x-amzn-sagemaker-target-variant", target));
        }
        let answer = invoke(&router, &endpoint, &headers, body.as_bytes()).await;

        assert_eq!(
            answer.status,
            200,
            "case {case:?}: an InService endpoint's invocation must be served, got {} with {}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.header("x-amzn-invoked-production-variant"),
            row.invoked,
            "case {case:?}: the answer must name the variant that served it, got {}",
            answer.header("x-amzn-invoked-production-variant")
        );
        let declared = answer.header("content-type");
        assert_eq!(
            declared, row.content_type,
            "case {case:?}: the answer must declare the content type its bytes carry, got \
             {declared:?}"
        );
        // A TEXTUAL rendering's bytes are only readable against the encoding they were written
        // in, so a text media type names it — and the bytes really are written in it.
        if declared.starts_with("text/") {
            assert!(
                declared.to_ascii_lowercase().contains("charset=utf-8"),
                "case {case:?}: a textual answer declares the character set it is written in, \
                 got {declared:?}"
            );
            String::from_utf8(answer.body.clone()).unwrap_or_else(|error| {
                panic!("case {case:?}: the answer is not the text it declares: {error}")
            });
        }

        // The body is the inference the artifact's OWN parameters imply, one prediction per
        // record in the order the payload carried them.
        let served = predictions(&answer, case);
        assert_eq!(
            served.len(),
            row.records.len(),
            "case {case:?}: one prediction per record, got {served:?}"
        );
        for (record, value) in row.records.iter().zip(&served) {
            let wanted = expected_prediction(record);
            assert!(
                (value - wanted).abs() < PREDICTION_TOLERANCE,
                "case {case:?}: record {record:?} must be answered {wanted}, got {value}"
            );
        }

        // Invoking is a read of the deployed model: a second identical invocation is answered
        // identically, headers included, so nothing the first call did changed what the next
        // caller is served.
        let again = invoke(&router, &endpoint, &headers, body.as_bytes()).await;
        assert_eq!(
            (
                again.status,
                again.header("content-type"),
                again.header("x-amzn-invoked-production-variant"),
                again.text(),
            ),
            (answer.status, declared, row.invoked.to_string(), answer.text()),
            "case {case:?}: the same invocation must be answered the same way"
        );
    }
}
