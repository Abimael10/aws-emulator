// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two refusals SageMaker's notebook-instance operations
//! word themselves: the name nothing is stored under, and the transition the instance's
//! current status does not admit.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness and the
//! `CreateNotebookInstance` request shape are this crate's shared ones.
//!
//! The rules under test:
//!
//! * `DescribeNotebookInstance` refuses a `NotebookInstanceName` this account holds no
//!   instance under — never created, or already deleted — with `ValidationException`/400
//!   whose WHOLE message is the fixed token `RecordNotFound`, with nothing interpolated from
//!   the request or the store, so the sentinel a client compares against is what it reads;
//!   the operations that would MOVE the instance (the stop, the delete) word the same fact
//!   with that token and the ARN the name resolves to;
//! * an instance whose status does not admit the transition a call asks for is refused
//!   `ValidationException`/400 stating the current status, the statuses the transition is
//!   taken from AS A BRACKETED LIST, the status it could not move to and the instance's ARN,
//!   and the instance is left exactly as it was.
//!
//! Both tables span what the rules generalise over rather than one recorded call: the
//! operation that resolves the name, the name's shape, the Region the request is signed for,
//! whether the name was never created or was deleted, and which transition is asked of which
//! status.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    notebook_instance_record_not_found_at, NOTEBOOK_INSTANCE_RECORD_NOT_FOUND,
};

use crate::inference_deployment_lifecycle::{error_code, message, Emulator};
use crate::notebook_instance_deletion::{notebook_instance_arn, notebook_instance_resting_in};
use crate::notebook_instance_lifecycle::create_body;

/// The operations that resolve a `NotebookInstanceName` and refuse one they hold nothing
/// under, and the message each of them words that fact with: the READER answers the fixed
/// token, and the two TRANSITIONS answer it with the ARN the name resolves to.
///
/// The presigned-URL mint is NOT among them: it words the same fact with its own recorded
/// sentence, which `resource_precondition_gates` pins.
fn absent_refusals(region: &str, name: &str) -> Vec<(&'static str, String)> {
    let at_the_arn = notebook_instance_record_not_found_at(&notebook_instance_arn(region, name));
    vec![
        (
            "DescribeNotebookInstance",
            NOTEBOOK_INSTANCE_RECORD_NOT_FOUND.to_string(),
        ),
        ("StopNotebookInstance", at_the_arn.clone()),
        ("DeleteNotebookInstance", at_the_arn),
    ]
}

/// The refusal a transition guard states: the current status, the statuses the transition is
/// taken from as a bracketed list, the status it could not reach, and the instance's ARN. A
/// literal on purpose: this suite is what pins the wording, so it does not ask the
/// implementation what it is.
fn transition_refusal(status: &str, allowed: &[&str], target: &str, arn: &str) -> String {
    format!(
        "Status ({status}) not in ([{}]). Unable to transition to ({target}) for Notebook \
         Instance ({arn}).",
        allowed.join(", ")
    )
}

/// RULE: `DescribeNotebookInstance` refuses a name this account holds no instance under
/// with `ValidationException`/400 whose whole message is the fixed `RecordNotFound` token,
/// and the two transitions refuse it with that token and the ARN the name resolves to —
/// whatever the name's shape, whichever Region the request is signed for, and whether the
/// name was never created or has just been deleted.
#[tokio::test]
async fn an_absent_notebook_instance_is_refused_with_its_operations_own_sentence() {
    // The rows differ along what the rule generalises over: the shapes
    // `NotebookInstanceName`'s modelled `len 0..63` / `^[a-zA-Z0-9](-*[a-zA-Z0-9])*$` admit
    // (one character, hyphenated, digits only, the 63-character maximum), and the Region.
    let longest = "n".repeat(63);
    let rows: Vec<(&str, &str, String)> = vec![
        ("a single-character name", "us-east-1", "a".to_string()),
        (
            "a hyphenated name",
            "eu-west-1",
            "never-created-notebook".to_string(),
        ),
        ("digits only", "ap-southeast-2", "2024".to_string()),
        (
            "the longest name the length bound admits",
            "us-west-2",
            longest,
        ),
    ];

    let emulator = Emulator::new();
    for (case, region, name) in &rows {
        for (resolver, expected) in absent_refusals(region, name) {
            let (status, refused) = emulator
                .call_in(region, resolver, &json!({ "NotebookInstanceName": name }))
                .await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "ValidationException"),
                "case {case:?}/{resolver}: a name nothing is stored under must be refused \
                 400 ValidationException, got {status} with {refused}"
            );
            assert_eq!(
                message(&refused),
                expected,
                "case {case:?}/{resolver}: expected {expected:?}, got {:?}",
                message(&refused)
            );
        }
    }

    // The same answers for a name that WAS held: an instance created, stopped and deleted is
    // as absent as one never created, so the message a delete-then-poll loop matches on is
    // the one its own operation words that fact with.
    let region = "us-east-1";
    let name = "deleted-notebook";
    let (status, created) = emulator
        .call_in(region, "CreateNotebookInstance", &create_body(name, &json!({})))
        .await;
    assert_eq!(
        status, 200,
        "case a deleted instance: CreateNotebookInstance must be served, got {status} with \
         {created}"
    );
    notebook_instance_resting_in(&emulator, "a deleted instance", name, "InService").await;
    let (status, stopped) = emulator
        .call_in(
            region,
            "StopNotebookInstance",
            &json!({ "NotebookInstanceName": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case a deleted instance: StopNotebookInstance must be served, got {status} with \
         {stopped}"
    );
    notebook_instance_resting_in(&emulator, "a deleted instance", name, "Stopped").await;
    let (status, deleted) = emulator
        .call_in(
            region,
            "DeleteNotebookInstance",
            &json!({ "NotebookInstanceName": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case a deleted instance: DeleteNotebookInstance must be served, got {status} with \
         {deleted}"
    );
    for (resolver, expected) in absent_refusals(region, name) {
        let (status, refused) = emulator
            .call_in(region, resolver, &json!({ "NotebookInstanceName": name }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case a deleted instance/{resolver}: expected a 400 ValidationException, got \
             {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case a deleted instance/{resolver}: expected {expected:?}, got {:?}",
            message(&refused)
        );
    }
}

/// One row of the transition-guard table: the status the instance is in when the call lands,
/// the call whose guard answers, and what its refusal has to name.
struct GuardRow {
    case: &'static str,
    region: &'static str,
    /// The status the instance is driven to before the call, through the service's own calls.
    status_when_called: &'static str,
    operation: &'static str,
    /// The statuses that operation's transition is taken from, as the refusal lists them.
    allowed: Vec<&'static str>,
    /// The status the refused call could not move the instance to.
    target: &'static str,
}

/// RULE: a transition a notebook instance's current status does not admit is refused with
/// the current status, the allowed set AS A BRACKETED LIST, the status the call could not
/// reach and the instance's ARN — and the instance keeps the status it had.
#[tokio::test]
async fn a_transition_the_status_does_not_admit_names_the_allowed_set_as_a_list() {
    // The rows differ along what the rule generalises over: which operation's guard answers,
    // the status the instance is in when the call lands, and so the allowed set and the
    // target status the refusal names.
    let rows: Vec<GuardRow> = vec![
        GuardRow {
            case: "a delete while the instance serves Jupyter",
            region: "us-east-1",
            status_when_called: "InService",
            operation: "DeleteNotebookInstance",
            allowed: vec!["Stopped", "Failed"],
            target: "Deleting",
        },
        GuardRow {
            case: "a stop of an instance that is already stopped",
            region: "eu-west-1",
            status_when_called: "Stopped",
            operation: "StopNotebookInstance",
            allowed: vec!["Pending", "InService"],
            target: "Stopping",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let GuardRow {
            case,
            region,
            status_when_called,
            operation,
            allowed,
            target,
        } = row;
        let emulator = Emulator::new();
        let name = format!("guarded-notebook-{index}");
        let (status, created) = emulator
            .call_in(
                region,
                "CreateNotebookInstance",
                &create_body(&name, &json!({})),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateNotebookInstance must be served, got {status} with {created}"
        );
        // The instance is driven to the status the row needs through the service's own
        // calls, and the CONDITION is what is waited for rather than a span.
        notebook_instance_resting_in(&emulator, case, &name, "InService").await;
        if *status_when_called == "Stopped" {
            let (status, stopped) = emulator
                .call_in(
                    region,
                    "StopNotebookInstance",
                    &json!({ "NotebookInstanceName": name }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: StopNotebookInstance must be served, got {status} with \
                 {stopped}"
            );
            notebook_instance_resting_in(&emulator, case, &name, "Stopped").await;
        }

        let (status, refused) = emulator
            .call_in(region, operation, &json!({ "NotebookInstanceName": name }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: the transition must be refused 400 ValidationException, got \
             {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            transition_refusal(
                status_when_called,
                allowed,
                target,
                &notebook_instance_arn(region, &name)
            ),
            "case {case:?}: expected the allowed set as a bracketed list, got {:?}",
            message(&refused)
        );

        // The refused call left the instance exactly as it was.
        let described: Value =
            notebook_instance_resting_in(&emulator, case, &name, status_when_called).await;
        assert_eq!(
            described.get("NotebookInstanceStatus"),
            Some(&json!(status_when_called)),
            "case {case:?}: expected the refused call to move nothing, got {described}"
        );
    }
}
