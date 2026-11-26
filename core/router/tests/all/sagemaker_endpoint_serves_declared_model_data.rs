// SPDX-License-Identifier: CC0-1.0

//! An endpoint serves the model its container DECLARED, in whichever form that container
//! declared it.
//!
//! The rule under test: a model's container names where its model data is and how that data is
//! prepared — `ContainerDefinition$ModelDataUrl` ("the S3 path where the model artifacts … are
//! stored[;] this path must point to a single gzip compressed tar archive") or
//! `ContainerDefinition$ModelDataSource`, whose `S3DataSource` states the path together with its
//! `S3DataType` (`S3Object` = "an object that is the ML model data to deploy", `S3Prefix` = "a key
//! name prefix[;] SageMaker uses all objects that match the specified key name prefix") and its
//! `CompressionType` (`Gzip` = "an object that is a gzip-compressed TAR archive", `None` = "an
//! uncompressed ML model") — and a deployment lays the files of the model directory out as the
//! declared form says: an archive's entries, the single file an object's key's last part names, or
//! every object under the prefix trimmed by it. So an `InService` endpoint answers HTTP 200 with
//! what the model behind its variant computes, naming that variant in
//! `x-Amzn-Invoked-Production-Variant`, for EVERY form its container may have declared; the
//! modelled 424 `ModelError` is reserved for a model that answered nothing, which is what a
//! container declaring no model data at all has.
//!
//! The rows differ along what the rule generalises over: which member declares the model data
//! (`ModelDataUrl`, `ModelDataSource`), the `S3DataType` and `CompressionType` the source declares
//! (`S3Object`/`Gzip`, `S3Object`/`None`, `S3Prefix`/`None`), what the model data carries (the
//! model's own inference code beside the parameters it loads, or the model's parameters alone),
//! how many objects hold it (one archive, one file, a tree of two), the payload and the media
//! types it is offered and asked for in.
//!
//! Only a cross-service suite can ask this: the flow spans the same four services over ONE engine
//! that `core/router/src/lib.rs` mounts — IAM for the execution role, Amazon S3 for the model
//! data, the SageMaker control plane to deploy and the SageMaker runtime to invoke. The harness
//! (the emulator, the request builders, the role and the poll-until-settled reading) is the
//! serving suite's ([`crate::sagemaker_endpoint_serves_a_trained_model`]) rather than a second
//! copy of it.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    model_archive, CSV_ANSWER_CONTENT_TYPE, JSON_MEDIA_TYPE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_role, image, invoke, send, settled, BUCKET,
};

/// The variant every endpoint below hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// `S3ModelDataSource$S3DataType`'s members
/// (`smithy_query(query="shape",shape="S3ModelDataType")`).
const S3_OBJECT: &str = "S3Object";
const S3_PREFIX: &str = "S3Prefix";

/// `S3ModelDataSource$CompressionType`'s members
/// (`smithy_query(query="shape",shape="ModelCompressionType")`).
const GZIP: &str = "Gzip";
const UNCOMPRESSED: &str = "None";

/// The identifier the parameters in the model directory carry, which the model's own code answers
/// with — so an answer carrying it is an answer computed from the model data that was deployed.
const MODEL_ID: &str = "declared-model-data-v1";

/// The handler script a framework inference container runs out of the model data's `code`
/// directory: its `model_fn` loads the parameters the model directory holds and its
/// `transform_fn` answers what the payload's own instances imply, beside the media types the
/// request offered and asked for.
const ECHO_SCRIPT: &str = r#"
import json
import os


def model_fn(model_dir):
    with open(os.path.join(model_dir, "parameters.json")) as held:
        return json.load(held)


def transform_fn(model, input_data, content_type, accept):
    payload = json.loads(input_data.decode("utf-8"))
    instances = payload["instances"]
    answered = {
        "model_id": model["model_id"],
        "received_content_type": content_type,
        "received_accept": accept,
        "received_instances": instances,
        "sum": sum(sum(row) for row in instances),
    }
    return json.dumps(answered), "application/json"
"#;

/// The parameters the script's own `model_fn` loads out of the model directory.
fn parameters() -> Vec<u8> {
    json!({ "model_id": MODEL_ID }).to_string().into_bytes()
}

/// The files the scripted model data holds: the handler script in its `code` directory and the
/// parameters beside it.
fn scripted_files() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "code/inference.py".to_string(),
            ECHO_SCRIPT.as_bytes().to_vec(),
        ),
        ("parameters.json".to_string(), parameters()),
    ]
}

/// Those same files as ONE gzip-compressed tar archive, which is what both `ModelDataUrl` and a
/// source declaring `Gzip` name.
fn scripted_archive() -> Vec<u8> {
    model_archive(&scripted_files()).expect("the model archive is built")
}

/// The model document an UNCOMPRESSED object carries: the parameters a run of this emulator
/// fitted, which this service computes a prediction from record by record.
fn fitted_document() -> Vec<u8> {
    json!({ "model": { "intercept": 1.0, "weights": [2.0, 3.0], "epochs": 4 } })
        .to_string()
        .into_bytes()
}

/// What one row's invocation must be answered with.
enum Answered {
    /// The JSON document the model's own code returned, under the media type it named.
    Code(Value),
    /// The comma-separated predictions a model served from its parameters answers.
    Parameters(&'static str),
    /// Nothing computed an inference: the modelled 424 `ModelError`.
    NoInference,
}

/// One row of the declared-form table.
struct Row {
    case: &'static str,
    /// The objects the row holds in Amazon S3, as `(key under the bucket, contents)`.
    objects: Vec<(String, Vec<u8>)>,
    /// The model-data members the row's `PrimaryContainer` declares.
    declared: Value,
    /// The payload one invocation carries, and the media types it is offered and asked for in.
    payload: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    expected: Answered,
}

/// The JSON document the script answers for `instances`, whose sum is `sum`, when the request
/// offered and asked for `application/json`.
fn code_answer(instances: Value, sum: f64) -> Value {
    json!({
        "model_id": MODEL_ID,
        "received_content_type": JSON_MEDIA_TYPE,
        "received_accept": JSON_MEDIA_TYPE,
        "received_instances": instances,
        "sum": sum,
    })
}

/// The media types a JSON row offers and asks for.
fn json_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("content-type", JSON_MEDIA_TYPE),
        ("accept", JSON_MEDIA_TYPE),
    ]
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "ModelDataUrl names one gzip-compressed tar archive",
            objects: vec![("url/model.tar.gz".to_string(), scripted_archive())],
            declared: json!({ "ModelDataUrl": format!("s3://{BUCKET}/url/model.tar.gz") }),
            payload: r#"{"instances": [[1.0, 2.0, 3.0]]}"#,
            headers: json_headers(),
            expected: Answered::Code(code_answer(json!([[1.0, 2.0, 3.0]]), 6.0)),
        },
        Row {
            case: "ModelDataSource names one object prepared as Gzip",
            objects: vec![("gzip/artifact.tar.gz".to_string(), scripted_archive())],
            declared: json!({
                "ModelDataSource": {
                    "S3DataSource": {
                        "S3Uri": format!("s3://{BUCKET}/gzip/artifact.tar.gz"),
                        "S3DataType": S3_OBJECT,
                        "CompressionType": GZIP,
                    },
                },
            }),
            // A different payload than the row above, so the answer is computed from this
            // invocation rather than replayed from the previous one.
            payload: r#"{"instances": [[2.5, 4.0]]}"#,
            headers: json_headers(),
            expected: Answered::Code(code_answer(json!([[2.5, 4.0]]), 6.5)),
        },
        Row {
            case: "ModelDataSource names a key name prefix holding the files uncompressed",
            objects: vec![
                (
                    "prefix/model/code/inference.py".to_string(),
                    ECHO_SCRIPT.as_bytes().to_vec(),
                ),
                ("prefix/model/parameters.json".to_string(), parameters()),
            ],
            declared: json!({
                "ModelDataSource": {
                    "S3DataSource": {
                        "S3Uri": format!("s3://{BUCKET}/prefix/model/"),
                        "S3DataType": S3_PREFIX,
                        "CompressionType": UNCOMPRESSED,
                    },
                },
            }),
            payload: r#"{"instances": [[1.0, 2.0], [3.0, 4.5]]}"#,
            headers: json_headers(),
            expected: Answered::Code(code_answer(json!([[1.0, 2.0], [3.0, 4.5]]), 10.5)),
        },
        Row {
            case: "ModelDataSource names one uncompressed object carrying the model itself",
            objects: vec![("plain/model.json".to_string(), fitted_document())],
            declared: json!({
                "ModelDataSource": {
                    "S3DataSource": {
                        "S3Uri": format!("s3://{BUCKET}/plain/model.json"),
                        "S3DataType": S3_OBJECT,
                        "CompressionType": UNCOMPRESSED,
                    },
                },
            }),
            // 1 + 2*2 + 3*3, the score the deployed parameters imply for the record.
            payload: "2,3",
            headers: vec![("content-type", "text/csv")],
            expected: Answered::Parameters("14"),
        },
        Row {
            case: "a container declaring no model data at all",
            objects: Vec::new(),
            declared: json!({}),
            payload: r#"{"instances": [[1.0]]}"#,
            headers: json_headers(),
            expected: Answered::NoInference,
        },
    ]
}

/// `PUT /<bucket>/<key>` with raw bytes — `PutObject` for model data that is not text.
async fn given_bytes(router: &MultiServiceRouter, case: &str, key: &str, body: Vec<u8>) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}/{key}"),
        &[],
        body,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the object {key} must be stored, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Deploy a model whose `PrimaryContainer` declares `declared` behind `endpoint`, under the
/// execution role `role_arn`, and answer once the endpoint is `InService`.
async fn deploy_declared(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    endpoint: &str,
    role_arn: &str,
    declared: &Value,
) {
    let mut container = json!({ "Image": image() });
    for (member, value) in declared.as_object().expect("the row declares an object") {
        container[member.as_str()] = value.clone();
    }
    control_ok(
        router,
        "CreateModel",
        &json!({
            "ModelName": name,
            "PrimaryContainer": container,
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    let config = format!("{name}-config");
    control_ok(
        router,
        "CreateEndpointConfig",
        &json!({
            "EndpointConfigName": config,
            "ProductionVariants": [{
                "VariantName": VARIANT,
                "ModelName": name,
                "InstanceType": "ml.m5.large",
                "InitialInstanceCount": 1,
            }],
        }),
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

/// THE RULE: an endpoint serves the model its container declared, in whichever form it declared
/// it, and names the variant that served it.
#[tokio::test]
async fn an_endpoint_serves_the_model_data_its_container_declared() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        for (key, body) in &row.objects {
            given_bytes(&router, case, key, body.clone()).await;
        }
        let name = format!("declared-model-{index}");
        let endpoint = format!("declared-endpoint-{index}");
        deploy_declared(&router, case, &name, &endpoint, &role_arn, &row.declared).await;

        let answer = invoke(&router, &endpoint, &row.headers, row.payload.as_bytes()).await;
        match &row.expected {
            Answered::Code(expected) => {
                assert_eq!(
                    answer.status,
                    200,
                    "case {case:?}: the invocation must be served, got {} with {}",
                    answer.status,
                    answer.text()
                );
                assert_eq!(
                    answer.json(),
                    *expected,
                    "case {case:?}: the answer is what the model's own code computed, got {}",
                    answer.text()
                );
                assert_eq!(
                    answer.header("content-type"),
                    JSON_MEDIA_TYPE,
                    "case {case:?}: the answer carries the media type the code named, got {}",
                    answer.text()
                );
            }
            Answered::Parameters(expected) => {
                assert_eq!(
                    answer.status,
                    200,
                    "case {case:?}: the invocation must be served, got {} with {}",
                    answer.status,
                    answer.text()
                );
                assert_eq!(
                    answer.text(),
                    *expected,
                    "case {case:?}: the answer is the prediction the deployed parameters imply, \
                     got {}",
                    answer.text()
                );
                assert_eq!(
                    answer.header("content-type"),
                    CSV_ANSWER_CONTENT_TYPE,
                    "case {case:?}: the answer's content type, got {}",
                    answer.text()
                );
            }
            Answered::NoInference => {
                assert_eq!(
                    (answer.status, answer.error_code().as_str()),
                    (424, "ModelError"),
                    "case {case:?}: a container with no model data computes nothing, got {} with \
                     {}",
                    answer.status,
                    answer.text()
                );
                assert_eq!(
                    answer.message(),
                    format!(
                        "Model {name} carries no ModelDataUrl, so there are no model artifacts \
                         to compute an inference from."
                    ),
                    "case {case:?}: the refusal must name the model that declared none"
                );
                continue;
            }
        }
        assert_eq!(
            answer.header("x-amzn-invoked-production-variant"),
            VARIANT,
            "case {case:?}: the answer must name the variant that served it, got {}",
            answer.text()
        );

        // Serving is a READ of the model data: the same payload is answered the same way again,
        // and the endpoint is untouched by having served.
        let again = invoke(&router, &endpoint, &row.headers, row.payload.as_bytes()).await;
        assert_eq!(
            (again.status, again.text()),
            (answer.status, answer.text()),
            "case {case:?}: the same payload must be answered the same way twice"
        );
        let described = control_ok(
            &router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint }),
        )
        .await;
        assert_eq!(
            described.get("EndpointStatus").and_then(Value::as_str),
            Some("InService"),
            "case {case:?}: the endpoint is untouched by serving, got {described}"
        );
    }
}

/// The form is the CONTAINER's declaration, not a guess about the bytes: the very same objects
/// declared as a prefix and as the single object they hold serve the model, while a declaration
/// naming a path nothing holds computes nothing.
#[tokio::test]
async fn the_declared_form_decides_which_objects_make_up_the_model_directory() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;
    given_bytes(
        &router,
        "the prefix's own objects",
        "both/model/code/inference.py",
        ECHO_SCRIPT.as_bytes().to_vec(),
    )
    .await;
    given_bytes(
        &router,
        "the prefix's own objects",
        "both/model/parameters.json",
        parameters(),
    )
    .await;

    // The ONE object that is the whole model data: the script's own file alone, placed under the
    // last part of its key — so the model directory holds no `code` directory for the container
    // to take an entry point out of, and nothing computes an inference.
    deploy_declared(
        &router,
        "one object of the same tree",
        "one-object-model",
        "one-object-endpoint",
        &role_arn,
        &json!({
            "ModelDataSource": {
                "S3DataSource": {
                    "S3Uri": format!("s3://{BUCKET}/both/model/code/inference.py"),
                    "S3DataType": S3_OBJECT,
                    "CompressionType": UNCOMPRESSED,
                },
            },
        }),
    )
    .await;
    let alone = invoke(
        &router,
        "one-object-endpoint",
        &json_headers(),
        br#"{"instances": [[1.0]]}"#,
    )
    .await;
    assert_eq!(
        (alone.status, alone.error_code().as_str()),
        (424, "ModelError"),
        "case \"one object of the same tree\": one object is one file of the model directory, so \
         the script's own code directory is not there, got {} with {}",
        alone.status,
        alone.text()
    );

    // The same objects declared as the PREFIX they live under are the whole model directory.
    deploy_declared(
        &router,
        "the prefix over the same objects",
        "whole-tree-model",
        "whole-tree-endpoint",
        &role_arn,
        &json!({
            "ModelDataSource": {
                "S3DataSource": {
                    "S3Uri": format!("s3://{BUCKET}/both/model/"),
                    "S3DataType": S3_PREFIX,
                    "CompressionType": UNCOMPRESSED,
                },
            },
        }),
    )
    .await;
    let whole = invoke(
        &router,
        "whole-tree-endpoint",
        &json_headers(),
        br#"{"instances": [[1.5, 2.5]]}"#,
    )
    .await;
    assert_eq!(
        whole.status,
        200,
        "case \"the prefix over the same objects\": the prefix's objects are the model \
         directory, got {} with {}",
        whole.status,
        whole.text()
    );
    assert_eq!(
        whole.json(),
        code_answer(json!([[1.5, 2.5]]), 4.0),
        "case \"the prefix over the same objects\": the answer is computed from the deployed \
         files, got {}",
        whole.text()
    );
}
