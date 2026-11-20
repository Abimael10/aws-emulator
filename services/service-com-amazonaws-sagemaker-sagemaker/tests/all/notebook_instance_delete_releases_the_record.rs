// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for what `DeleteNotebookInstance` TAKES AWAY: the one stored
//! record every notebook-instance call of this service resolves a `NotebookInstanceName`
//! through.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness is this crate's shared
//! one, from `tests/all/inference_deployment_lifecycle.rs`; the notebook-instance request
//! shape and the poll-until-rested reader come from the suites that own them.
//!
//! `tests/all/notebook_instance_deletion.rs` covers WHICH statuses the delete accepts and
//! that the reader no longer resolves the name. These tests cover the rest of the rule —
//! "SageMaker AI removes the ML compute instance, and deletes the ML storage volume and the
//! network interface associated with the notebook instance" (`DeleteNotebookInstance`'s own
//! doc), i.e. nothing of the instance is left behind — over a TABLE of inputs:
//!
//! * every call that resolves a notebook-instance reference refuses the deleted name
//!   afterwards, the tag set the record carried is gone with it, and a presigned session
//!   minted while it was serving opens nothing;
//! * the name is free again, so a later create admits it and yields a record carrying the
//!   NEW request's configuration and tags and nothing of the deleted one's.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state as sm;

use super::inference_deployment_lifecycle::{
    error_code, f64_field, message, str_field, Emulator,
};
use super::notebook_instance_deletion::{
    delete_notebook_instance, notebook_instance_arn, notebook_instance_resting_in,
};
use super::notebook_instance_lifecycle::create_body;

/// A second `InstanceType` to re-create a deleted name with, from the modelled
/// `InstanceType` enum (`smithy_query shape=InstanceType`) — one the suite that creates
/// notebook instances does not use, so a reader replaying it can only be reporting the
/// second request.
const OTHER_INSTANCE: &str = "ml.m5.xlarge";

/// The sentence this service states a notebook instance it holds nothing for with, on the
/// calls that address the instance itself (the presigned-URL mint) or its tag set — the
/// crate's own helper, so this suite pins the service's wording rather than a copy of it.
fn absent_sentence(arn: &str) -> String {
    sm::notebook_instance_does_not_exist(arn)
}

/// `CreateNotebookInstance` for `name` in `region`, carrying `tags` and whatever `extra`
/// holds beside the required members.
async fn create(
    emulator: &Emulator,
    region: &str,
    name: &str,
    tags: &Value,
    extra: &Value,
) -> (u16, Value) {
    let mut body = create_body(name, extra);
    body["Tags"] = tags.clone();
    emulator
        .call_in(region, "CreateNotebookInstance", &body)
        .await
}

/// Stop the notebook instance `name` and read it back until the clock's `Stopped`
/// transition is materialised, which is the status a delete needs.
async fn stop_and_rest(emulator: &Emulator, case: &str, region: &str, name: &str) {
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
    notebook_instance_resting_in(emulator, case, name, "Stopped").await;
}

/// `ListTags` for `arn` in `region`.
async fn list_tags(emulator: &Emulator, region: &str, arn: &str) -> (u16, Value) {
    emulator
        .call_in(region, "ListTags", &json!({ "ResourceArn": arn }))
        .await
}

/// `CreatePresignedNotebookInstanceUrl` for `name` in `region`.
async fn presign(emulator: &Emulator, region: &str, name: &str) -> (u16, Value) {
    emulator
        .call_in(
            region,
            "CreatePresignedNotebookInstanceUrl",
            &json!({ "NotebookInstanceName": name }),
        )
        .await
}

/// RULE: the delete removes the instance's whole record, so afterwards every call that
/// resolves a notebook-instance reference refuses the name, the tag set the record carried
/// is no longer reachable, and a presigned session minted against it opens nothing.
#[tokio::test]
async fn deleting_a_notebook_instance_releases_every_reference_it_was_resolved_through() {
    // The rows differ along what the rule generalises over: the signed region, how many
    // tags the record carried (none, one, several) and how much configuration it held.
    let rows: Vec<(&str, &str, Value, Value)> = vec![
        (
            "an instance carrying one tag",
            "us-east-1",
            json!([{ "Key": "team", "Value": "research" }]),
            json!({}),
        ),
        (
            "an instance carrying several tags and a full configuration",
            "eu-west-1",
            json!([
                { "Key": "team", "Value": "research" },
                { "Key": "cost-centre", "Value": "4711" },
            ]),
            json!({
                "SubnetId": "subnet-0abc1234",
                "SecurityGroupIds": ["sg-00000001"],
                "VolumeSizeInGB": 120,
                "RootAccess": "Disabled",
                "DirectInternetAccess": "Disabled",
            }),
        ),
        (
            "an instance carrying no tag at all",
            "ap-southeast-2",
            json!([]),
            json!({ "PlatformIdentifier": "notebook-al2-v2" }),
        ),
    ];

    for (index, (case, region, tags, extra)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("released-notebook-{index}");
        let arn = notebook_instance_arn(region, &name);

        let (status, created) = create(&emulator, region, &name, &tags, &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateNotebookInstance, got {status} with \
             {created}"
        );
        notebook_instance_resting_in(&emulator, case, &name, "InService").await;

        // While the record is there, its tag set is reachable by ARN and a presigned
        // session can be minted and opened: that is what the delete has to take away.
        let (status, listed) = list_tags(&emulator, region, &arn).await;
        assert_eq!(
            (status, &listed["Tags"]),
            (200, &tags),
            "case {case:?}: expected the stored tag set before the delete, got {status} with \
             {listed}"
        );
        let (status, minted) = presign(&emulator, region, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreatePresignedNotebookInstanceUrl while \
             the instance serves Jupyter, got {status} with {minted}"
        );
        let url = str_field(&minted, "AuthorizedUrl");
        let (status, body) = emulator.fetch(&url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the minted URL to open a session before the delete, got \
             {status} with {body:?}"
        );

        stop_and_rest(&emulator, case, region, &name).await;

        // A STOPPED instance is still held: the mint is refused for serving no Jupyter
        // server, which is a different fact from the one every call states after the
        // delete — so the assertions below are about the record being gone, not stopped.
        let (status, stopped_refusal) = presign(&emulator, region, &name).await;
        assert_eq!(
            (status, error_code(&stopped_refusal).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a stopped instance's mint to be refused, got {status} \
             with {stopped_refusal}"
        );
        assert_ne!(
            message(&stopped_refusal),
            absent_sentence(&arn),
            "case {case:?}: expected a stopped instance to still be held, got {:?}",
            message(&stopped_refusal)
        );

        let (status, deleted) = delete_notebook_instance(&emulator, region, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteNotebookInstance, got {status} with \
             {deleted}"
        );

        // Every call that resolves a notebook-instance reference now refuses the name: the
        // reader, the stop, the presigned-URL mint and each tag call that addresses its ARN.
        let refused: Vec<(&str, &str, Value)> = vec![
            (
                "the reader",
                "DescribeNotebookInstance",
                json!({ "NotebookInstanceName": name }),
            ),
            (
                "the stop",
                "StopNotebookInstance",
                json!({ "NotebookInstanceName": name }),
            ),
            (
                "the presigned-URL mint",
                "CreatePresignedNotebookInstanceUrl",
                json!({ "NotebookInstanceName": name }),
            ),
            ("the tag reader", "ListTags", json!({ "ResourceArn": arn })),
            (
                "the tag writer",
                "AddTags",
                json!({
                    "ResourceArn": arn,
                    "Tags": [{ "Key": "after", "Value": "delete" }],
                }),
            ),
            (
                "the tag remover",
                "DeleteTags",
                json!({ "ResourceArn": arn, "TagKeys": ["team"] }),
            ),
        ];
        for (subject, target, body) in refused {
            let (status, answer) = emulator.call_in(region, target, &body).await;
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, "ValidationException"),
                "case {case:?}/{subject}: expected {target} to refuse the deleted instance, \
                 got {status} with {answer}"
            );
            assert!(
                answer.get("NotebookInstanceArn").is_none() && answer.get("Tags").is_none(),
                "case {case:?}/{subject}: expected {target} to report nothing of the deleted \
                 instance, got {answer}"
            );
        }

        // The session the mint published while the instance served Jupyter opens nothing
        // once the record it was granted against is gone.
        let (status, body) = emulator.fetch(&url).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the published session to authenticate nothing after the \
             delete, got {status} with {body:?}"
        );

        // ...and the refused tag write left no tag set behind, so the ARN still resolves to
        // no resource at all.
        let (status, listed) = list_tags(&emulator, region, &arn).await;
        assert_eq!(
            (status, message(&listed)),
            (400, absent_sentence(&arn)),
            "case {case:?}: expected the deleted instance's ARN to hold no tag set, got \
             {status} with {listed}"
        );
    }
}

/// RULE: the delete frees the name it removed, so a later `CreateNotebookInstance` admits
/// it — while a name a record is still held under is refused — and the instance that create
/// yields carries the NEW request's configuration and tags and nothing the deleted one held.
#[tokio::test]
async fn a_deleted_notebook_instances_name_is_free_for_a_new_instance() {
    // The rows differ along what the rule generalises over: the signed region and which
    // members the second create words differently from the first (instance type, volume
    // size, tag set, including re-creating with no tag at all).
    let rows: Vec<(&str, &str, Value, Value, Value, Value)> = vec![
        (
            "a new instance type and a new tag set",
            "us-east-1",
            json!([{ "Key": "generation", "Value": "first" }]),
            json!({ "VolumeSizeInGB": 5 }),
            json!([{ "Key": "generation", "Value": "second" }]),
            json!({ "InstanceType": OTHER_INSTANCE, "VolumeSizeInGB": 20 }),
        ),
        (
            "a new volume size and no tag at all",
            "eu-west-1",
            json!([
                { "Key": "generation", "Value": "first" },
                { "Key": "owner", "Value": "research" },
            ]),
            json!({ "VolumeSizeInGB": 120, "RootAccess": "Disabled" }),
            json!([]),
            json!({ "VolumeSizeInGB": 16384 }),
        ),
    ];

    for (index, (case, region, first_tags, first_extra, second_tags, second_extra)) in
        rows.into_iter().enumerate()
    {
        let emulator = Emulator::new();
        let name = format!("reused-notebook-{index}");
        let arn = notebook_instance_arn(region, &name);

        let (status, created) = create(&emulator, region, &name, &first_tags, &first_extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the first CreateNotebookInstance, got \
             {status} with {created}"
        );
        let first = notebook_instance_resting_in(&emulator, case, &name, "InService").await;

        // While the record is held, the name is taken: the same create is refused.
        let (status, taken) = create(&emulator, region, &name, &second_tags, &second_extra).await;
        assert_eq!(
            (status, error_code(&taken).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a create of a name already held to be refused, got \
             {status} with {taken}"
        );
        assert!(
            message(&taken).contains(&arn),
            "case {case:?}: expected the refusal to name the instance already there, got {:?}",
            message(&taken)
        );

        stop_and_rest(&emulator, case, region, &name).await;
        let (status, deleted) = delete_notebook_instance(&emulator, region, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteNotebookInstance, got {status} with \
             {deleted}"
        );

        // The name is free: the create the store refused a moment ago is now served.
        let (status, recreated) =
            create(&emulator, region, &name, &second_tags, &second_extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the freed name to be admitted again, got {status} with \
             {recreated}"
        );
        assert_eq!(
            str_field(&recreated, "NotebookInstanceArn"),
            arn,
            "case {case:?}: expected the re-created instance's own ARN, got {recreated:?}"
        );

        // What the reader reports is the SECOND request's instance: its configuration, its
        // tags, a creation instant no earlier than the first one's, and a status the
        // deleted record was not left in.
        let again = emulator
            .ok(
                "DescribeNotebookInstance",
                &json!({ "NotebookInstanceName": name }),
            )
            .await;
        let first_instance_type = str_field(&first, "InstanceType");
        let expected_instance_type = second_extra
            .get("InstanceType")
            .and_then(Value::as_str)
            .unwrap_or(first_instance_type.as_str())
            .to_string();
        assert_eq!(
            str_field(&again, "InstanceType"),
            expected_instance_type,
            "case {case:?}: expected the re-created instance to report the second request's \
             instance type, got {again:?}"
        );
        assert_eq!(
            again["VolumeSizeInGB"], second_extra["VolumeSizeInGB"],
            "case {case:?}: expected the re-created instance to report the second request's \
             volume size, got {again:?}"
        );
        assert_ne!(
            str_field(&again, "NotebookInstanceStatus"),
            "Stopped",
            "case {case:?}: expected a status the deleted record was not left in, got \
             {again:?}"
        );
        assert!(
            f64_field(&again, "CreationTime") >= f64_field(&first, "CreationTime"),
            "case {case:?}: expected the re-created instance's own creation instant, got {} \
             against {}",
            f64_field(&again, "CreationTime"),
            f64_field(&first, "CreationTime")
        );
        for member in ["RootAccess", "DirectInternetAccess", "SubnetId"] {
            if first_extra.get(member).is_some() && second_extra.get(member).is_none() {
                assert_ne!(
                    again.get(member),
                    first_extra.get(member),
                    "case {case:?}: expected nothing of the deleted instance's {member} to \
                     survive, got {again:?}"
                );
            }
        }

        // The tag set is the second request's alone: the deleted record's tags went with it.
        let (status, listed) = list_tags(&emulator, region, &arn).await;
        assert_eq!(
            (status, &listed["Tags"]),
            (200, &second_tags),
            "case {case:?}: expected only the second request's tags, got {status} with \
             {listed}"
        );
    }
}
