// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the ONE fact SageMaker's notebook-instance operations word
//! two ways: a `NotebookInstanceName` this account holds no instance under.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness and the
//! `CreateNotebookInstance` request shape are this crate's shared ones.
//!
//! The RULE under test: the notebook-instance READER answers the bare sentinel token
//! `RecordNotFound`, carrying nothing of the request, while every operation that ACTS on
//! the instance — the stop, the delete, the presigned-URL mint and the tag calls that
//! address its ARN — names the resource it could not act on,
//! `Notebook Instance <arn> does not exist`, with the ARN the requested name resolves to in
//! the Region the request signed for. Both are `ValidationException` on HTTP 400, because
//! none of these operations models an error.
//!
//! The tables span what the rule generalises over rather than the one recorded call: which
//! operation resolves the name, the name's shape inside the modelled
//! `len 0..63` / `^[a-zA-Z0-9](-*[a-zA-Z0-9])*$`, the Region the request is signed for, and
//! whether the name was never created or was created, stopped and deleted.

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, Emulator, ACCOUNT};
use crate::notebook_instance_deletion::notebook_instance_resting_in;
use crate::notebook_instance_lifecycle::create_body;

/// The whole message the notebook-instance READER answers for a name nothing is stored
/// under. A literal on purpose: this suite is what pins the wording, so it does not ask the
/// implementation what it is. It interpolates NOTHING — it is the sentinel a client compares
/// against to tell "this notebook is gone" from any other bad request.
const READER_SENTINEL: &str = "RecordNotFound";

/// The whole message every notebook-instance operation that ACTS on the instance answers
/// for a name nothing is stored under. A literal on purpose, for the same reason: the fixed
/// text is `Notebook Instance ` and ` does not exist`, and the only variable part is the ARN
/// the requested name resolves to.
fn acting_sentence(arn: &str) -> String {
    format!("Notebook Instance {arn} does not exist")
}

/// The ARN `NotebookInstanceArn`'s documented form gives the instance `name` in `region`.
fn notebook_instance_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:notebook-instance/{name}")
}

/// The operations that resolve a notebook-instance reference and the message each words an
/// absent instance with, for `name` in `region`: the reader's sentinel, and the acting
/// sentence for the stop, the delete, the presigned-URL mint and each tag call.
fn absent_refusals(region: &str, name: &str) -> Vec<(&'static str, Value, String)> {
    let arn = notebook_instance_arn(region, name);
    let by_name = json!({ "NotebookInstanceName": name });
    vec![
        (
            "DescribeNotebookInstance",
            by_name.clone(),
            READER_SENTINEL.to_string(),
        ),
        (
            "StopNotebookInstance",
            by_name.clone(),
            acting_sentence(&arn),
        ),
        (
            "DeleteNotebookInstance",
            by_name.clone(),
            acting_sentence(&arn),
        ),
        (
            "CreatePresignedNotebookInstanceUrl",
            by_name,
            acting_sentence(&arn),
        ),
        (
            "ListTags",
            json!({ "ResourceArn": arn }),
            acting_sentence(&arn),
        ),
        (
            "AddTags",
            json!({
                "ResourceArn": arn,
                "Tags": [{ "Key": "after", "Value": "delete" }],
            }),
            acting_sentence(&arn),
        ),
        (
            "DeleteTags",
            json!({ "ResourceArn": arn, "TagKeys": ["team"] }),
            acting_sentence(&arn),
        ),
    ]
}

/// Drive every refusal of `name` in `region` and assert each operation's own sentence.
async fn assert_absent_everywhere(emulator: &Emulator, case: &str, region: &str, name: &str) {
    for (operation, body, expected) in absent_refusals(region, name) {
        let (status, refused) = emulator.call_in(region, operation, &body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}/{operation}: a name nothing is stored under must be refused 400 \
             ValidationException, got {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}/{operation}: expected {expected:?}, got {:?}",
            message(&refused)
        );
    }
}

/// RULE: for a `NotebookInstanceName` this account holds no instance under, the READER
/// answers the bare sentinel token and every operation that ACTS on the instance names the
/// resource it could not act on in the `does not exist` sentence — whatever the name's shape
/// and whichever Region the request is signed for.
#[tokio::test]
async fn an_absent_notebook_instance_is_worded_by_what_the_operation_does() {
    // The rows differ along what the rule generalises over: the shapes
    // `NotebookInstanceName`'s modelled `len 0..63` / `^[a-zA-Z0-9](-*[a-zA-Z0-9])*$` admits
    // (one character, hyphenated, double-hyphenated, digits only, the 63-character
    // maximum), and the Region the request is signed for.
    let longest = "n".repeat(63);
    let rows: Vec<(&str, &str, String)> = vec![
        ("a single-character name", "us-east-1", "a".to_string()),
        (
            "a hyphenated name",
            "eu-west-1",
            "never-created-notebook".to_string(),
        ),
        (
            "an inner double hyphen",
            "ap-southeast-2",
            "never--created".to_string(),
        ),
        ("digits only", "us-west-2", "2024".to_string()),
        (
            "the longest name the length bound admits",
            "eu-central-1",
            longest,
        ),
    ];

    let emulator = Emulator::new();
    for (case, region, name) in &rows {
        assert_absent_everywhere(&emulator, case, region, name).await;
    }
}

/// RULE: a name that WAS held is as absent as one never created once the delete has taken
/// the record away — so the clean-up a client drives (stop, delete, then a repeat of either
/// call) reads the acting operations' own `does not exist` sentence, while the reader keeps
/// answering the sentinel.
#[tokio::test]
async fn a_deleted_notebook_instance_is_worded_the_same_way_as_one_never_created() {
    // The rows differ along the Region signed for and how much configuration the deleted
    // record carried, which is what the rule generalises over: none of it reaches either
    // sentence.
    let rows: Vec<(&str, &str, Value)> = vec![
        ("a bare instance", "us-east-1", json!({})),
        (
            "a configured instance",
            "eu-west-1",
            json!({
                "SubnetId": "subnet-0abc1234",
                "SecurityGroupIds": ["sg-00000001"],
                "VolumeSizeInGB": 120,
                "RootAccess": "Disabled",
                "DirectInternetAccess": "Disabled",
            }),
        ),
        (
            "an instance carrying tags",
            "ap-southeast-2",
            json!({ "Tags": [{ "Key": "team", "Value": "research" }] }),
        ),
    ];

    for (index, (case, region, extra)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("cleaned-up-notebook-{index}");

        let (status, created) = emulator
            .call_in(region, "CreateNotebookInstance", &create_body(&name, &extra))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateNotebookInstance, got {status} with \
             {created}"
        );
        notebook_instance_resting_in(&emulator, case, &name, "InService").await;

        let (status, stopped) = emulator
            .call_in(
                region,
                "StopNotebookInstance",
                &json!({ "NotebookInstanceName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StopNotebookInstance, got {status} with \
             {stopped}"
        );
        notebook_instance_resting_in(&emulator, case, &name, "Stopped").await;

        let (status, deleted) = emulator
            .call_in(
                region,
                "DeleteNotebookInstance",
                &json!({ "NotebookInstanceName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteNotebookInstance, got {status} with \
             {deleted}"
        );

        // The record is gone, so every operation words the one fact its own way — and a
        // second pass answers the same thing, because a refusal moves nothing.
        assert_absent_everywhere(&emulator, case, region, &name).await;
        assert_absent_everywhere(&emulator, case, region, &name).await;
    }
}
