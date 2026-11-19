// SPDX-License-Identifier: CC0-1.0

//! What a model artifact carrying the model's own INFERENCE CODE serves, at the one reading that
//! takes a model out of an artifact's bytes (`state::artifact_from_bytes`) and the one inference
//! every surface of a deployed model goes through (`state::serve_artifact`) — and end to end
//! through a batch transform job over the real wire path, which is what a client sees.
//!
//! RULE: a SageMaker model is served by its container, and a framework inference container takes
//! the model's own handler script out of the artifact's `code` directory — the entry point the
//! container's `SAGEMAKER_PROGRAM` names, `inference.py` by convention — and calls that script's
//! own functions for every request: `model_fn` loads the model out of the model directory,
//! `input_fn` reads the payload, `predict_fn` computes the prediction and `output_fn` renders the
//! answer, with the single `transform_fn` replacing the last three. So what such a model answers
//! is what its own code RETURNED, computed by running it; an artifact carrying no entry point the
//! container runs answers nothing at all, and a script that raises fails the request with its own
//! exception.
//!
//! The rows differ along what the rule generalises over: which of the handler functions the
//! script defines, whether it loads parameters out of the model directory, which entry point the
//! container names, what media type the answer is rendered in, how many records the payload
//! carries, and how a run ends (an answer, an exception, a bound).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    artifact_from_bytes, model_archive, serve_artifact, ServedArtifact, ServingRefusal,
    CSV_MEDIA_TYPE, JSON_MEDIA_TYPE,
};

use super::model_artifact_admission::{str_field, trust_for, Emulator, ACCOUNT, REGION};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The handler script of the recorded shape: every one of the four functions the container calls,
/// each handing its input straight on, which is the identity transform a passthrough model is.
const IDENTITY_SCRIPT: &str = r#"
def model_fn(model_dir):
    return {}


def input_fn(input_data, content_type):
    return input_data


def predict_fn(input_data, model):
    return input_data


def output_fn(prediction, accept):
    return prediction
"#;

/// A script that really computes: it reads the payload's records, scales each record's sum by the
/// parameter its own `model_fn` returned, and renders one prediction per line.
const COMPUTING_SCRIPT: &str = r#"
def model_fn(model_dir):
    return 2.0


def input_fn(input_data, content_type):
    rows = []
    for line in input_data.decode("utf-8").splitlines():
        if line.strip():
            rows.append([float(value) for value in line.split(",")])
    return rows


def predict_fn(rows, model):
    return [sum(row) * model for row in rows]


def output_fn(predictions, accept):
    return "\n".join(str(prediction) for prediction in predictions) + "\n"
"#;

/// A script defining the SINGLE handler that replaces the last three, and naming the media type
/// it rendered its answer in.
const TRANSFORM_FN_SCRIPT: &str = r#"
def model_fn(model_dir):
    return None


def transform_fn(model, input_data, content_type, accept):
    return input_data.decode("utf-8").upper(), "text/plain"
"#;

/// A script whose `model_fn` loads its parameters out of the model directory the artifact carries,
/// and which defines no `output_fn` at all — so what it answers is the value `predict_fn`
/// returned.
const MODEL_DIRECTORY_SCRIPT: &str = r#"
import json
import os


def model_fn(model_dir):
    with open(os.path.join(model_dir, "parameters.json")) as held:
        return json.load(held)


def predict_fn(input_data, model):
    scored = []
    for value in input_data.decode("utf-8").split(","):
        if value.strip():
            scored.append(model["bias"] + model["weight"] * float(value))
    return scored
"#;

/// A script that raises: the model's own failure, which no answer may be invented for.
const RAISING_SCRIPT: &str = r#"
def predict_fn(input_data, model):
    raise ValueError("this model cannot score that record")
"#;

/// The artifact `entries` make up, as Amazon S3 holds it.
fn artifact(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let entries: Vec<(String, Vec<u8>)> = entries
        .iter()
        .map(|(path, body)| ((*path).to_string(), body.clone()))
        .collect();
    model_archive(&entries).expect("the archive is built")
}

/// An artifact whose code directory holds `source` at `name`, beside the files `files` names.
fn scripted_artifact(name: &str, source: &str, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut entries: Vec<(&str, Vec<u8>)> =
        vec![(name, source.as_bytes().to_vec())];
    entries.extend(files.iter().cloned());
    artifact(&entries)
}

/// One row of the script table.
struct Row {
    case: &'static str,
    /// The artifact's bytes.
    artifact: Vec<u8>,
    /// The entry point the model's container names, when it names one.
    program: Option<&'static str>,
    /// The payload one invocation carries.
    payload: &'static str,
    /// The media type the payload is in.
    content_type: Option<&'static str>,
    /// The media type the request asks the answer in.
    accept: Option<&'static str>,
    /// What the script's own code answers for that payload.
    expected: &'static str,
    /// The media type the answer carries.
    media_type: &'static str,
}

fn rows() -> Vec<Row> {
    let parameters = json!({ "bias": 1.0, "weight": 3.0 }).to_string().into_bytes();
    vec![
        Row {
            case: "the four handler functions, handing the payload straight on",
            artifact: scripted_artifact("code/inference.py", IDENTITY_SCRIPT, &[]),
            program: None,
            payload: "1,2\n3,4\n",
            content_type: Some(CSV_MEDIA_TYPE),
            accept: None,
            expected: "1,2\n3,4\n",
            media_type: CSV_MEDIA_TYPE,
        },
        Row {
            case: "one record through the same script",
            artifact: scripted_artifact("code/inference.py", IDENTITY_SCRIPT, &[]),
            program: None,
            payload: "7\n",
            content_type: Some(CSV_MEDIA_TYPE),
            accept: None,
            expected: "7\n",
            media_type: CSV_MEDIA_TYPE,
        },
        Row {
            case: "a script that computes a prediction per record",
            artifact: scripted_artifact("code/inference.py", COMPUTING_SCRIPT, &[]),
            program: None,
            payload: "1,2\n3,4\n",
            content_type: Some(CSV_MEDIA_TYPE),
            accept: None,
            // (1+2)*2 and (3+4)*2, as the script renders them.
            expected: "6.0\n14.0\n",
            media_type: CSV_MEDIA_TYPE,
        },
        Row {
            case: "the single handler that replaces the last three, naming its media type",
            artifact: scripted_artifact("code/inference.py", TRANSFORM_FN_SCRIPT, &[]),
            program: None,
            payload: "ab\ncd\n",
            content_type: Some("text/plain"),
            accept: Some("text/plain"),
            expected: "AB\nCD\n",
            media_type: "text/plain",
        },
        Row {
            case: "a script loading its parameters out of the model directory",
            artifact: scripted_artifact(
                "code/inference.py",
                MODEL_DIRECTORY_SCRIPT,
                &[("parameters.json", parameters)],
            ),
            program: None,
            payload: "2,4",
            content_type: Some(CSV_MEDIA_TYPE),
            accept: None,
            // 1 + 3*2 and 1 + 3*4, as the value the script returned is rendered.
            expected: "[7.0, 13.0]",
            media_type: JSON_MEDIA_TYPE,
        },
        Row {
            case: "the entry point the container's SAGEMAKER_PROGRAM names",
            artifact: scripted_artifact("code/serve_model.py", COMPUTING_SCRIPT, &[]),
            program: Some("serve_model.py"),
            payload: "5\n",
            content_type: Some(CSV_MEDIA_TYPE),
            accept: None,
            expected: "10.0\n",
            media_type: CSV_MEDIA_TYPE,
        },
    ]
}

/// THE RULE: what a model whose artifact carries its own inference code answers is what that code
/// returned, computed by RUNNING it.
#[test]
fn the_inference_a_scripted_artifact_serves_is_what_its_own_code_returned() {
    for row in rows() {
        let case = row.case;
        let loaded = artifact_from_bytes(&row.artifact, row.program)
            .unwrap_or_else(|| panic!("case {case:?}: the artifact carries inference code"));
        assert!(
            matches!(loaded, ServedArtifact::Script(_)),
            "case {case:?}: an artifact carrying code is served by that code, got {loaded:?}"
        );
        let served = serve_artifact(
            &loaded,
            row.payload.as_bytes(),
            row.content_type,
            row.accept,
        )
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: the script serves the payload, got {refusal:?}")
        });
        let answered = String::from_utf8(served.body.clone())
            .unwrap_or_else(|error| panic!("case {case:?}: the answer is text: {error}"));
        assert_eq!(
            answered, row.expected,
            "case {case:?}: expected {:?}, got {answered:?}",
            row.expected
        );
        assert_eq!(
            served.content_type, row.media_type,
            "case {case:?}: expected the answer in {:?}, got {:?}",
            row.media_type, served.content_type
        );
    }
}

/// RULE: a script that RAISES fails the request with its own exception — no answer is invented for
/// a run that computed none.
#[test]
fn a_script_that_raises_serves_nothing_and_reports_its_own_exception() {
    let bytes = scripted_artifact("code/inference.py", RAISING_SCRIPT, &[]);
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let refusal = serve_artifact(&loaded, b"1,2\n", Some(CSV_MEDIA_TYPE), None)
        .expect_err("a script that raises answers nothing");
    let ServingRefusal::ScriptRaised {
        error_type,
        error_message,
    } = &refusal
    else {
        panic!("expected the script's own exception, got {refusal:?}");
    };
    assert_eq!(
        error_type, "ValueError",
        "expected the exception the script raised, got {refusal:?}"
    );
    assert!(
        error_message.contains("cannot score that record"),
        "expected the message the script raised, got {error_message:?}"
    );
    assert!(
        refusal.message().contains("ValueError"),
        "the reported condition names the exception, got {:?}",
        refusal.message()
    );
}

/// RULE: what a run REPORTS about itself is a string the script chose, so it is clamped before it
/// reaches a response or the store — a script raising a message of its own choosing cannot make one
/// request carry an unbounded one, and the whole condition still fits the modelled member it is
/// reported through.
#[test]
fn the_text_a_script_raises_is_clamped_before_it_leaves_the_run() {
    let source = "def predict_fn(input_data, model):\n    raise ValueError(\"A\" * 200000)\n";
    let bytes = scripted_artifact("code/inference.py", source, &[]);
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let refusal = serve_artifact(&loaded, b"1\n", Some(CSV_MEDIA_TYPE), None)
        .expect_err("a script that raises answers nothing");
    let ServingRefusal::ScriptRaised {
        error_type,
        error_message,
    } = &refusal
    else {
        panic!("expected the script's own exception, got {refusal:?}");
    };
    let bound = service_com_amazonaws_sagemaker_sagemaker::state::MAX_SCRIPT_DETAIL_BYTES;
    assert!(
        error_message.len() == bound && error_type == "ValueError",
        "expected the raised text clamped to {bound} bytes, got {} bytes of {error_type:?}",
        error_message.len()
    );
    // `DescribeTransformJob$FailureReason` is @length 0..1024, and a batch transform job reports
    // this condition through it, so the whole composed condition has to fit that length.
    assert!(
        refusal.message().len() <= FAILURE_REASON_MAXIMUM,
        "the whole condition must fit the modelled {FAILURE_REASON_MAXIMUM} characters, got {}",
        refusal.message().len()
    );
}

/// `FailureReason`'s modelled maximum (`@length 0..1024`), which a transform job reports a script's
/// own failure through.
const FAILURE_REASON_MAXIMUM: usize = 1024;

/// RULE: the media type an answer carries is one the client can be handed — the script's own when
/// it named a media type, the request's when it asked for one particular type, and otherwise the
/// type a SageMaker inference container answers records in. A value that is no media type names
/// none.
///
/// The rows differ along who named the type and along how a named value falls short: a bare token,
/// a control character, and a length past what the member admits.
#[test]
fn the_media_type_an_answer_carries_is_one_a_client_may_be_handed() {
    // (case, what the script's output_fn names, what the request asks for, the type answered)
    let rows: [(&str, &str, Option<&str>, &str); 7] = [
        ("the script's own type", "\"text/plain\"", None, "text/plain"),
        (
            "a type carrying a parameter",
            "\"text/csv; charset=utf-8\"",
            None,
            "text/csv; charset=utf-8",
        ),
        (
            "no type named, and a request asking for one",
            "None",
            Some(JSON_MEDIA_TYPE),
            JSON_MEDIA_TYPE,
        ),
        ("no type named and none asked for", "None", None, CSV_MEDIA_TYPE),
        (
            "a request asking for any type at all",
            "None",
            Some("*/*"),
            CSV_MEDIA_TYPE,
        ),
        ("a named value that is no media type", "\"csv\"", None, CSV_MEDIA_TYPE),
        (
            "a named value carrying a line break",
            "\"text/csv\\r\\nX-Injected: 1\"",
            None,
            CSV_MEDIA_TYPE,
        ),
    ];
    for (case, named, accept, expected) in rows {
        let source = format!(
            "def predict_fn(input_data, model):\n    return input_data\n\n\ndef output_fn(prediction, accept):\n    return prediction, {named}\n"
        );
        let bytes = scripted_artifact("code/inference.py", &source, &[]);
        let loaded = artifact_from_bytes(&bytes, None)
            .unwrap_or_else(|| panic!("case {case:?}: the artifact carries inference code"));
        let served = serve_artifact(&loaded, b"1\n", Some(CSV_MEDIA_TYPE), accept)
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the script serves the payload, got {refusal:?}")
            });
        assert_eq!(
            served.content_type, expected,
            "case {case:?}: expected the answer in {expected:?}, got {:?}",
            served.content_type
        );
    }
    // And a named type longer than the member admits names none either.
    let long = "a".repeat(service_com_amazonaws_sagemaker_sagemaker::state::MAX_MEDIA_TYPE_BYTES);
    let source = format!(
        "def predict_fn(input_data, model):\n    return input_data\n\n\ndef output_fn(prediction, accept):\n    return prediction, \"text/{long}\"\n"
    );
    let bytes = scripted_artifact("code/inference.py", &source, &[]);
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let served = serve_artifact(&loaded, b"1\n", Some(CSV_MEDIA_TYPE), None)
        .expect("the script serves the payload");
    assert_eq!(
        served.content_type, CSV_MEDIA_TYPE,
        "a type past the length the member admits names none, got {:?}",
        served.content_type
    );
}

/// RULE: a run of a script is BOUNDED in host code the script cannot reach — a script that
/// computes without end is stopped and the request is answered with that bound, never with an
/// answer nothing computed.
#[test]
fn a_script_that_computes_without_end_is_stopped_by_its_own_bounds() {
    let source = "def predict_fn(input_data, model):\n    while True:\n        pass\n";
    let bytes = scripted_artifact("code/inference.py", source, &[]);
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let refusal = serve_artifact(&loaded, b"1\n", Some(CSV_MEDIA_TYPE), None)
        .expect_err("a script that never returns answers nothing");
    assert!(
        matches!(refusal, ServingRefusal::ScriptStopped { .. }),
        "expected the run to be stopped by one of its bounds, got {refusal:?}"
    );
}

/// One row of the artifact-reading table: the case, the artifact's bytes, the entry point the
/// model's container names, and what the artifact is read as — `None` for an artifact nothing
/// serves, `Some(false)` for one served from its own parameters, `Some(true)` for one served by
/// its own code.
struct ReadRow {
    case: &'static str,
    artifact: Vec<u8>,
    program: Option<&'static str>,
    scripted: Option<bool>,
}

/// RULE: an artifact carrying neither a model's parameters nor an entry point the container runs
/// serves nothing at all, and an artifact carrying parameters is served from those parameters.
///
/// The rows differ along how the artifact falls short of carrying code the container runs, along
/// carrying both parameters and code, and along the form the container names the entry point in.
#[test]
fn an_artifact_is_read_as_what_it_carries() {
    let fitted = json!({
        "model": { "type": "linear", "intercept": 1.0, "weights": [2.0], "epochs": 1 },
    })
    .to_string()
    .into_bytes();
    let rows: Vec<ReadRow> = vec![
        ReadRow {
            case: "a code directory holding no entry point of that name",
            artifact: scripted_artifact("code/helpers.py", IDENTITY_SCRIPT, &[]),
            program: None,
            scripted: None,
        },
        ReadRow {
            case: "a container naming an entry point the artifact does not carry",
            artifact: scripted_artifact("code/inference.py", IDENTITY_SCRIPT, &[]),
            program: Some("serve_model.py"),
            scripted: None,
        },
        ReadRow {
            case: "a script outside the code directory",
            artifact: scripted_artifact("inference.py", IDENTITY_SCRIPT, &[]),
            program: None,
            scripted: None,
        },
        ReadRow {
            case: "an artifact holding nothing but data",
            artifact: artifact(&[("notes.txt", b"no parameters and no code here".to_vec())]),
            program: None,
            scripted: None,
        },
        ReadRow {
            case: "an artifact carrying the model's own parameters",
            artifact: artifact(&[("model.json", fitted.clone())]),
            program: None,
            scripted: Some(false),
        },
        ReadRow {
            case: "an artifact carrying parameters beside a script",
            artifact: artifact(&[
                ("model.json", fitted),
                ("code/inference.py", IDENTITY_SCRIPT.as_bytes().to_vec()),
            ]),
            program: None,
            scripted: Some(false),
        },
        ReadRow {
            case: "a container naming the entry point with its code directory",
            artifact: scripted_artifact("code/inference.py", IDENTITY_SCRIPT, &[]),
            program: Some("code/inference.py"),
            scripted: Some(true),
        },
        ReadRow {
            case: "a container naming the entry point without its suffix",
            artifact: scripted_artifact("code/serve_model.py", IDENTITY_SCRIPT, &[]),
            program: Some("serve_model"),
            scripted: Some(true),
        },
    ];
    for row in rows {
        let (case, expected) = (row.case, row.scripted);
        let read = artifact_from_bytes(&row.artifact, row.program);
        let scripted = read
            .as_ref()
            .map(|loaded| matches!(loaded, ServedArtifact::Script(_)));
        assert_eq!(
            scripted, expected,
            "case {case:?}: expected {expected:?} (None: nothing serves it, Some(false): its own \
             parameters, Some(true): its own code), got {read:?}"
        );
    }
}

/// How long a transform job is polled for the status it has to reach, and how often. What the
/// poll waits for is the CONDITION, re-read in a loop, so a loaded host cannot fail a case the
/// clock would have served.
const SETTLE_BUDGET: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// RULE, end to end: a batch transform job whose model's artifact carries its own inference code
/// RUNS that code over every input object, writes what the code answered as that object's `.out`
/// and reports `Completed` with no `FailureReason`.
#[tokio::test]
async fn a_transform_job_runs_the_inference_code_the_artifact_carries() {
    let bucket = "scripted-transform";
    let role_name = "scripted-transform-role";
    let emulator = Emulator::new();
    let role = emulator
        .given_role(role_name, &trust_for(SERVICE_PRINCIPAL))
        .await;
    emulator
        .given_role_policy(
            role_name,
            &json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["s3:GetObject", "s3:PutObject", "s3:ListBucket"],
                    "Resource": [
                        format!("arn:aws:s3:::{bucket}"),
                        format!("arn:aws:s3:::{bucket}/*"),
                    ],
                }],
            }),
        )
        .await;
    emulator.given_bucket(bucket).await;
    // The artifact a framework container is deployed with: the handler script, in the code
    // directory the container takes it out of.
    let staged = scripted_artifact("code/inference.py", COMPUTING_SCRIPT, &[]);
    let (status, answered) = emulator
        .s3("PUT", &format!("/{bucket}/model/model.tar.gz"), &staged)
        .await;
    assert_eq!(
        status,
        200,
        "the artifact must be stored, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    // (the input object, the records it holds, what the script answers for them)
    let inputs: [(&str, &str, &str); 2] = [
        ("transform/input/one.csv", "1,2\n3,4\n", "6.0\n14.0\n"),
        ("transform/input/two.csv", "5\n", "10.0\n"),
    ];
    for (key, body, _) in inputs {
        emulator.given_object(bucket, key, body).await;
    }

    emulator
        .sagemaker_ok(
            "a scripted model",
            "CreateModel",
            &json!({
                "ModelName": "scriptedmodel",
                "ExecutionRoleArn": role,
                "PrimaryContainer": {
                    "Image": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/sagemaker-scikit-learn:1.2-1"),
                    "ModelDataUrl": format!("s3://{bucket}/model/model.tar.gz"),
                    "Environment": { "SAGEMAKER_PROGRAM": "inference.py" },
                },
            }),
        )
        .await;
    emulator
        .sagemaker_ok(
            "a scripted transform",
            "CreateTransformJob",
            &json!({
                "TransformJobName": "scriptedtransform",
                "ModelName": "scriptedmodel",
                "TransformInput": {
                    "DataSource": {
                        "S3DataSource": {
                            "S3DataType": "S3Prefix",
                            "S3Uri": format!("s3://{bucket}/transform/input/"),
                        },
                    },
                    "ContentType": "text/csv",
                    "SplitType": "Line",
                },
                "TransformOutput": { "S3OutputPath": format!("s3://{bucket}/transform/output/") },
                "TransformResources": { "InstanceType": "ml.m5.large", "InstanceCount": 1 },
            }),
        )
        .await;

    let deadline = Instant::now() + SETTLE_BUDGET;
    let settled = loop {
        let read = emulator
            .sagemaker_ok(
                "a scripted transform",
                "DescribeTransformJob",
                &json!({ "TransformJobName": "scriptedtransform" }),
            )
            .await;
        if str_field(&read, "TransformJobStatus") != "InProgress" {
            break read;
        }
        assert!(
            Instant::now() < deadline,
            "the transform job never left InProgress; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    };
    assert_eq!(
        str_field(&settled, "TransformJobStatus"),
        "Completed",
        "a job whose model's code ran over its input completes, got {settled}"
    );
    assert_eq!(
        settled.get("FailureReason"),
        None,
        "a completed job reports no failure reason, got {settled}"
    );
    for (key, _, expected) in inputs {
        let name = key.rsplit('/').next().unwrap_or(key);
        let path = format!("/{bucket}/transform/output/{name}.out");
        let (status, body) = emulator.s3("GET", &path, &[]).await;
        assert_eq!(
            status, 200,
            "the transform must write {path}, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        let written = String::from_utf8(body).expect("the written object is text");
        assert_eq!(
            written, expected,
            "object {key}: the .out holds what the model's own code answered, expected \
             {expected:?}, got {written:?}"
        );
    }
    // And nothing else was written under the output prefix: one object per input object.
    let (status, listing) = emulator
        .s3(
            "GET",
            &format!("/{bucket}?list-type=2&prefix=transform/output/"),
            &[],
        )
        .await;
    assert_eq!(
        status,
        200,
        "the output prefix must be listable, got {status} {}",
        String::from_utf8_lossy(&listing)
    );
    let listed = String::from_utf8_lossy(&listing).matches("<Key>").count();
    assert_eq!(
        listed,
        inputs.len(),
        "expected one .out per input object, got {listed} in {}",
        String::from_utf8_lossy(&listing)
    );
}

/// RULE: the answer a run may hand back and the payload it may be handed are bounded in host
/// code, so one request cannot make this process hold an unbounded amount.
#[test]
fn a_payload_larger_than_one_run_may_be_handed_is_refused() {
    let bytes = scripted_artifact("code/inference.py", IDENTITY_SCRIPT, &[]);
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let payload = vec![b'1'; service_com_amazonaws_sagemaker_sagemaker::state::MAX_SCRIPT_PAYLOAD_BYTES + 1];
    let refusal = serve_artifact(&loaded, &payload, Some(CSV_MEDIA_TYPE), None)
        .expect_err("a payload past the bound is refused");
    assert!(
        matches!(refusal, ServingRefusal::PayloadTooLarge { .. }),
        "expected the payload bound to refuse it, got {refusal:?}"
    );
}

/// The values this suite asserts against the model are read back as JSON where the script
/// answered JSON, so a row's expected text is the document the run really answered.
#[test]
fn the_json_answer_a_script_returns_is_a_document() {
    let parameters = json!({ "bias": 1.0, "weight": 3.0 }).to_string().into_bytes();
    let bytes = scripted_artifact(
        "code/inference.py",
        MODEL_DIRECTORY_SCRIPT,
        &[("parameters.json", parameters)],
    );
    let loaded =
        artifact_from_bytes(&bytes, None).expect("the artifact carries inference code");
    let served = serve_artifact(&loaded, b"2,4", Some(CSV_MEDIA_TYPE), None)
        .expect("the script serves the payload");
    let document: Value =
        serde_json::from_slice(&served.body).expect("the answer is a JSON document");
    assert_eq!(
        document,
        json!([7.0, 13.0]),
        "expected the predictions the script computed, got {document}"
    );
}
