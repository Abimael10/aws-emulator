// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two task runs that move a machine learning transform's LABELS
//! between the transform and Amazon S3: `StartImportLabelsTaskRun` and
//! `StartExportLabelsTaskRun`.
//!
//! RULE: a label task run of a transform this account holds carries its task out against
//! the emulator's own object store and is recorded on that transform. An import reads the
//! label file at the `InputS3Path`, folds its labels into the set the transform holds —
//! combining them with the labels already held, or, with `ReplaceAllLabels`, forgetting
//! those and learning only from the uploaded set — and moves the transform's `LabelCount`
//! to the labels it then holds. An export writes "all labeled data for a particular
//! transform" back out as one label file under the `OutputS3Path`. So the labels an import
//! stored are the labels an export writes, cell for cell. Each start mints a `TaskRunId`,
//! records the run in `STARTING` with the properties of its own task kind
//! (`IMPORT_LABELS`/`EXPORT_LABELS` and the path the request named), and the run comes to
//! rest `SUCCEEDED` when the clock owes it its end — or `FAILED`, carrying why, when the
//! object store could not serve the file, in which case the labels held do not move. A
//! `TransformId` this account holds no transform under earns the operations' modelled
//! `EntityNotFoundException` and a path that is no Amazon S3 path their modelled
//! `InvalidInputException`; neither starts a run.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the label
//! files the runs read are staged through Amazon S3's own `PutObject`, so what is asserted
//! is HTTP behaviour over state the data plane really holds.
//!
//! The rows differ along what the rule generalises over: how many labels the file holds
//! (none, one, several), what a cell holds (an ordinary value, the separator, a quote, a
//! newline, nothing), how many columns the header names, whether a second upload combines
//! or replaces, and the form of a path or a `TransformId` that addresses nothing.
//!
//! The due instant a run's terminal status falls at is moved THROUGH THE STORE rather than
//! slept for ([`crate::ml_task_runs::set_due_instant`]), so every transition here is
//! deterministic.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ML_TASK_RUN_STATUS_FAILED, ML_TASK_RUN_STATUS_STARTING, ML_TASK_RUN_STATUS_SUCCEEDED,
    ML_TASK_TYPE_EXPORT_LABELS, ML_TASK_TYPE_IMPORT_LABELS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::column_statistics_computation::emulator_with_s3;
use crate::job_runs::already_past;
use crate::ml_task_runs::set_due_instant;
use crate::ml_transforms::given_transform;
use crate::wire::{call, error_code, error_message, ok, s3_client, ACCOUNT_ID};

/// The bucket every label file below is staged in and exported to.
const BUCKET: &str = "glue-ml-labels";

/// Where the staged label file of one case lives.
fn input_path(case_key: &str) -> String {
    format!("s3://{BUCKET}/uploads/{case_key}.csv")
}

/// Where the export of one case writes: a prefix, which is what an export's path is.
fn output_path(case_key: &str) -> String {
    format!("s3://{BUCKET}/exports/{case_key}/")
}

/// The key prefix of [`output_path`], for the listing that finds what an export wrote.
fn output_prefix(case_key: &str) -> String {
    format!("exports/{case_key}/")
}

/// An emulator with Glue AND Amazon S3 on one engine, holding the bucket the label files
/// live in.
///
/// Amazon S3 is mounted because a label task run really reads and writes the label file:
/// it reaches the object store over the emulator's own in-process callback, so the object a
/// test stages with `PutObject` is the object the import reads, and the object an export
/// writes is one the same client can fetch. The harness is the shared one every suite that
/// stages objects behind Glue uses ([`emulator_with_s3`]).
async fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_s3();
    s3_client(&router)
        .create_bucket()
        .bucket(BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {BUCKET} must be created, got {error:?}"));
    (router, engine)
}

/// Stage one label file at `path` (an `s3://bucket/key` value).
async fn stage_label_file(router: &MultiServiceRouter, path: &str, body: &str) {
    let key = path
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| panic!("the staged path {path} must live in {BUCKET}"));
    s3_client(router)
        .put_object()
        .bucket(BUCKET)
        .key(key)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            body.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the label file {key} must be stored, got {error:?}"));
}

/// The keys under `prefix`, in the order the object store lists them.
async fn objects_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let listed = s3_client(router)
        .list_objects_v2()
        .bucket(BUCKET)
        .prefix(prefix)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the listing of {prefix} must answer, got {error:?}"));
    listed
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// The text of one object.
async fn object_text(router: &MultiServiceRouter, key: &str) -> String {
    let object = s3_client(router)
        .get_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the object {key} must be readable, got {error:?}"));
    let bytes = object
        .body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("the object {key} must arrive in full, got {error:?}"))
        .into_bytes();
    String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|error| panic!("the object {key} must be text, got {error}"))
}

/// Start an import of the labels at `path` into `transform` and answer the `TaskRunId`.
async fn start_import(
    router: &MultiServiceRouter,
    transform: &str,
    path: &str,
    replace: bool,
) -> String {
    let started = ok(
        router,
        "StartImportLabelsTaskRun",
        &json!({
            "TransformId": transform,
            "InputS3Path": path,
            "ReplaceAllLabels": replace,
        }),
    )
    .await;
    task_run_id("StartImportLabelsTaskRun", &started)
}

/// Start an export of `transform`'s labels under `path` and answer the `TaskRunId`.
async fn start_export(router: &MultiServiceRouter, transform: &str, path: &str) -> String {
    let started = ok(
        router,
        "StartExportLabelsTaskRun",
        &json!({ "TransformId": transform, "OutputS3Path": path }),
    )
    .await;
    task_run_id("StartExportLabelsTaskRun", &started)
}

/// The `TaskRunId` a start reports, which is the whole of a run's address beside its
/// transform.
fn task_run_id(operation: &str, started: &Value) -> String {
    started
        .get("TaskRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{operation} must answer a TaskRunId, got {started}"))
        .to_string()
}

/// What `GetMLTaskRun` reports for one run of one transform.
async fn get_run(router: &MultiServiceRouter, transform: &str, run_id: &str) -> Value {
    ok(
        router,
        "GetMLTaskRun",
        &json!({ "TransformId": transform, "TaskRunId": run_id }),
    )
    .await
}

/// The `LabelCount` a read of the transform reports.
async fn label_count(router: &MultiServiceRouter, transform: &str) -> i64 {
    let read = ok(
        router,
        "GetMLTransform",
        &json!({ "TransformId": transform }),
    )
    .await;
    read.get("LabelCount")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("GetMLTransform must report a LabelCount, got {read}"))
}

/// Assert that `run` is the task run of `task_type` the start recorded, in `status`, and
/// answer it.
fn assert_run(case: &str, run: &Value, task_type: &str, status: &str) {
    assert_eq!(
        run.get("Status").and_then(Value::as_str),
        Some(status),
        "case {case:?}: expected the run in {status}, got {run}"
    );
    assert_eq!(
        run.pointer("/Properties/TaskType").and_then(Value::as_str),
        Some(task_type),
        "case {case:?}: expected a {task_type} run, got {run}"
    );
}

/// Make the end the clock owes every unsettled run due, and read the run back.
async fn settled_run(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    transform: &str,
    run_id: &str,
) -> Value {
    set_due_instant(engine, already_past());
    get_run(router, transform, run_id).await
}

/// One row of the round-trip table: the case, its key in the object store, the label file
/// staged for it and how many LABELS that file holds — counted in the row rather than from
/// its lines, because a quoted cell may hold a newline of its own.
type FileRow<'a> = (&'a str, &'a str, &'a str, i64);

#[tokio::test]
async fn an_imported_label_file_round_trips_through_the_export() {
    // Every file is written in the canonical comma-separated form — a cell is quoted only
    // where it has to be, and every row ends in a newline — which is the form the label
    // file an export writes carries, so the round trip is compared byte for byte.
    let rows: &[FileRow<'_>] = &[
        (
            "one label",
            "one-label",
            "labeling_set_id,label,name\nset-1,0,alice\n",
            1,
        ),
        (
            "three labels",
            "three-labels",
            "labeling_set_id,label,name\nset-1,0,alice\nset-1,0,alicia\nset-1,1,bob\n",
            3,
        ),
        (
            "a header and no label at all",
            "header-only",
            "labeling_set_id,label,name\n",
            0,
        ),
        (
            "a cell holding the separator",
            "separator-cell",
            "labeling_set_id,label,name\nset-1,0,\"alice, a\"\n",
            1,
        ),
        (
            "a cell holding a quote",
            "quote-cell",
            "labeling_set_id,label,name\nset-1,0,\"alice \"\"a\"\"\"\n",
            1,
        ),
        (
            "a cell holding a newline",
            "newline-cell",
            "labeling_set_id,label,name\nset-1,0,\"alice\na\"\n",
            1,
        ),
        (
            "a cell holding nothing",
            "empty-cell",
            "labeling_set_id,label,name\nset-1,0,\n",
            1,
        ),
        (
            "a wider header",
            "wide-header",
            "labeling_set_id,label,id,first,last,town\nset-1,0,1,alice,a,ajo\n",
            1,
        ),
        (
            "columns of the labeller's own naming",
            "own-columns",
            "set,answer,who\ns1,yes,alice\n",
            1,
        ),
    ];

    for (case, key, file, labels) in rows {
        let (router, engine) = emulator().await;
        let transform = given_transform(&router, "labelled", Value::Null).await;
        let path = input_path(key);
        stage_label_file(&router, &path, file).await;

        let import = start_import(&router, &transform, &path, false).await;
        // The run is in flight when the start answers: the clock owes it its end.
        let started = get_run(&router, &transform, &import).await;
        assert_run(case, &started, ML_TASK_TYPE_IMPORT_LABELS, ML_TASK_RUN_STATUS_STARTING);
        assert_eq!(
            started
                .pointer("/Properties/ImportLabelsTaskRunProperties/InputS3Path")
                .and_then(Value::as_str),
            Some(path.as_str()),
            "case {case:?}: expected the run to carry the path it imported from, got {started}"
        );
        assert_eq!(
            started
                .pointer("/Properties/ImportLabelsTaskRunProperties/Replace")
                .and_then(Value::as_bool),
            Some(false),
            "case {case:?}: expected the run to carry the upload's Replace, got {started}"
        );

        let settled = settled_run(&router, &engine, &transform, &import).await;
        assert_run(case, &settled, ML_TASK_TYPE_IMPORT_LABELS, ML_TASK_RUN_STATUS_SUCCEEDED);
        assert!(
            settled.get("ErrorString").is_none(),
            "case {case:?}: a run that imported the labels failed on nothing, got {settled}"
        );

        // The labels the transform now holds are the file's rows, which is what its count
        // reports.
        let counted = label_count(&router, &transform).await;
        assert_eq!(
            counted, *labels,
            "case {case:?}: expected the transform to hold {labels} labels, got {counted}"
        );

        // The export writes those same labels back out, under the path it was given.
        let out = output_path(key);
        let export = start_export(&router, &transform, &out).await;
        let exported = settled_run(&router, &engine, &transform, &export).await;
        assert_run(case, &exported, ML_TASK_TYPE_EXPORT_LABELS, ML_TASK_RUN_STATUS_SUCCEEDED);
        assert_eq!(
            exported
                .pointer("/Properties/ExportLabelsTaskRunProperties/OutputS3Path")
                .and_then(Value::as_str),
            Some(out.as_str()),
            "case {case:?}: expected the run to carry the path it exported to, got {exported}"
        );

        let written = objects_under(&router, &output_prefix(key)).await;
        assert_eq!(
            written,
            vec![format!("{}{export}.csv", output_prefix(key))],
            "case {case:?}: expected the export to leave one file named for its run, got \
             {written:?}"
        );
        let text = object_text(&router, &written[0]).await;
        assert_eq!(
            text, *file,
            "case {case:?}: expected the exported file to hold the labels imported, got {text:?}"
        );
    }
}

/// One row of the combining table: the case, whether the second upload replaces, and the
/// file the export must then hold.
type CombineRow<'a> = (&'a str, bool, &'a str);

#[tokio::test]
async fn a_second_upload_combines_with_or_replaces_the_labels_held() {
    const FIRST: &str = "labeling_set_id,label,name\nset-1,0,alice\nset-1,1,bob\n";
    const SECOND: &str = "labeling_set_id,label,name\nset-2,0,carol\n";
    // Reordered columns: no cell may change column when the two files are combined.
    const REORDERED: &str = "label,labeling_set_id,name\n0,set-3,dave\n";

    let rows: &[CombineRow<'_>] = &[
        (
            "combining, which is the default",
            false,
            "labeling_set_id,label,name\nset-1,0,alice\nset-1,1,bob\nset-2,0,carol\n",
        ),
        ("replacing all labels", true, SECOND),
    ];

    for (case, replace, expected) in rows {
        let (router, engine) = emulator().await;
        let transform = given_transform(&router, "combined", Value::Null).await;
        let first = input_path("first");
        let second = input_path("second");
        stage_label_file(&router, &first, FIRST).await;
        stage_label_file(&router, &second, SECOND).await;

        start_import(&router, &transform, &first, false).await;
        let run = start_import(&router, &transform, &second, *replace).await;
        let settled = settled_run(&router, &engine, &transform, &run).await;
        assert_run(case, &settled, ML_TASK_TYPE_IMPORT_LABELS, ML_TASK_RUN_STATUS_SUCCEEDED);

        let out = output_path("combined");
        let export = start_export(&router, &transform, &out).await;
        let key = format!("{}{export}.csv", output_prefix("combined"));
        let text = object_text(&router, &key).await;
        assert_eq!(
            text, *expected,
            "case {case:?}: expected the export to hold {expected:?}, got {text:?}"
        );
        let counted = label_count(&router, &transform).await;
        let expected_labels = expected.lines().count().saturating_sub(1) as i64;
        assert_eq!(
            counted, expected_labels,
            "case {case:?}: expected the transform to hold {expected_labels} labels, got {counted}"
        );
    }

    // Combining a file whose columns are ordered differently keeps every label, each cell
    // under the column it was labelled for.
    let (router, engine) = emulator().await;
    let transform = given_transform(&router, "reordered", Value::Null).await;
    let first = input_path("first");
    let reordered = input_path("reordered");
    stage_label_file(&router, &first, FIRST).await;
    stage_label_file(&router, &reordered, REORDERED).await;
    start_import(&router, &transform, &first, false).await;
    let run = start_import(&router, &transform, &reordered, false).await;
    let settled = settled_run(&router, &engine, &transform, &run).await;
    assert_run(
        "reordered columns",
        &settled,
        ML_TASK_TYPE_IMPORT_LABELS,
        ML_TASK_RUN_STATUS_SUCCEEDED,
    );
    let out = output_path("reordered");
    let export = start_export(&router, &transform, &out).await;
    let key = format!("{}{export}.csv", output_prefix("reordered"));
    let text = object_text(&router, &key).await;
    assert_eq!(
        text, "label,labeling_set_id,name\n0,set-1,alice\n1,set-1,bob\n0,set-3,dave\n",
        "the labels already held must be carried over column by column, got {text:?}"
    );
}

#[tokio::test]
async fn a_label_file_the_object_store_does_not_serve_fails_the_run() {
    let (router, engine) = emulator().await;
    let transform = given_transform(&router, "unreadable", Value::Null).await;
    // Rows differ along WHY the object store serves no label file at that path: no such
    // object, no such bucket, and an object that is no label file at all.
    stage_label_file(&router, &input_path("empty-file"), "").await;
    let rows: &[(&str, String)] = &[
        ("no such object", input_path("never-staged")),
        (
            "no such bucket",
            "s3://glue-ml-labels-missing/uploads/labels.csv".to_string(),
        ),
        ("an object holding no header", input_path("empty-file")),
    ];

    for (case, path) in rows {
        let run = start_import(&router, &transform, path, false).await;
        let settled = settled_run(&router, &engine, &transform, &run).await;
        assert_run(case, &settled, ML_TASK_TYPE_IMPORT_LABELS, ML_TASK_RUN_STATUS_FAILED);
        let reason = settled
            .get("ErrorString")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !reason.is_empty(),
            "case {case:?}: a run that imported nothing must say why, got {settled}"
        );
        // Nothing was imported, so the transform holds the labels it held before.
        let counted = label_count(&router, &transform).await;
        assert_eq!(
            counted, 0,
            "case {case:?}: expected the transform to hold no label, got {counted}"
        );
    }

    // And the export of a transform nothing has labelled writes the header of a labelling
    // file over the columns it runs against, rather than nothing at all.
    let out = output_path("unlabelled");
    let export = start_export(&router, &transform, &out).await;
    let settled = settled_run(&router, &engine, &transform, &export).await;
    assert_run(
        "no label held",
        &settled,
        ML_TASK_TYPE_EXPORT_LABELS,
        ML_TASK_RUN_STATUS_SUCCEEDED,
    );
    let key = format!("{}{export}.csv", output_prefix("unlabelled"));
    let text = object_text(&router, &key).await;
    assert_eq!(
        text, "labeling_set_id,label\n",
        "expected the export of an unlabelled transform to write a labelling header, got {text:?}"
    );
}

/// One row of the refusal table: the case, the operation, the body sent, the error the
/// service must answer with and the message it must carry.
type RefusalRow<'a> = (&'a str, &'a str, Value, &'a str, String);

#[tokio::test]
async fn a_label_task_run_of_nothing_is_refused_and_starts_no_run() {
    let (router, _engine) = emulator().await;
    let transform = given_transform(&router, "refusals", Value::Null).await;
    let staged = input_path("refusals");
    stage_label_file(&router, &staged, "labeling_set_id,label\nset-1,0\n").await;

    // The `TransformId` forms that address no transform: one of the minted shape, one of
    // another shape, and the id of a transform that was deleted.
    let unknown = "0123456789abcdef0123456789abcdef";
    let missing_message = |id: &str| {
        format!("Cannot find MLTransform in account {ACCOUNT_ID} with handle {id}")
    };
    // The paths that are no Amazon S3 path: another scheme, a bare relative path, a value
    // naming no bucket, and — for an import, which reads ONE object — a path naming the
    // bucket alone.
    let bad_path = |member: &str, path: &str| {
        format!("{member} {path} is not a valid Amazon S3 path of the form s3://bucket/key")
    };

    let rows: &[RefusalRow<'_>] = &[
        (
            "an import of a transform that does not exist",
            "StartImportLabelsTaskRun",
            json!({ "TransformId": unknown, "InputS3Path": staged }),
            "EntityNotFoundException",
            missing_message(unknown),
        ),
        (
            "an export of a transform that does not exist",
            "StartExportLabelsTaskRun",
            json!({ "TransformId": unknown, "OutputS3Path": output_path("refused") }),
            "EntityNotFoundException",
            missing_message(unknown),
        ),
        (
            "an import from another scheme",
            "StartImportLabelsTaskRun",
            json!({ "TransformId": transform, "InputS3Path": "https://example.test/labels.csv" }),
            "InvalidInputException",
            bad_path("InputS3Path", "https://example.test/labels.csv"),
        ),
        (
            "an import from a relative path",
            "StartImportLabelsTaskRun",
            json!({ "TransformId": transform, "InputS3Path": "uploads/labels.csv" }),
            "InvalidInputException",
            bad_path("InputS3Path", "uploads/labels.csv"),
        ),
        (
            "an import from a path naming no bucket",
            "StartImportLabelsTaskRun",
            json!({ "TransformId": transform, "InputS3Path": "s3:///uploads/labels.csv" }),
            "InvalidInputException",
            bad_path("InputS3Path", "s3:///uploads/labels.csv"),
        ),
        (
            "an import from a path naming no object",
            "StartImportLabelsTaskRun",
            json!({ "TransformId": transform, "InputS3Path": format!("s3://{BUCKET}") }),
            "InvalidInputException",
            bad_path("InputS3Path", &format!("s3://{BUCKET}")),
        ),
        (
            "an export to another scheme",
            "StartExportLabelsTaskRun",
            json!({ "TransformId": transform, "OutputS3Path": "https://example.test/out/" }),
            "InvalidInputException",
            bad_path("OutputS3Path", "https://example.test/out/"),
        ),
        (
            "an export to a path naming no bucket",
            "StartExportLabelsTaskRun",
            json!({ "TransformId": transform, "OutputS3Path": "s3:///out/" }),
            "InvalidInputException",
            bad_path("OutputS3Path", "s3:///out/"),
        ),
    ];

    for (case, operation, body, expected_code, expected_message) in rows {
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled {expected_code}'s 400, got {status} with \
             {answered}"
        );
        assert!(
            error_code(&answered).ends_with(expected_code),
            "case {case:?}: expected {expected_code}, got {}",
            error_code(&answered)
        );
        assert_eq!(
            error_message(&answered),
            *expected_message,
            "case {case:?}: expected the refusal to state the member it is about, got {answered}"
        );
    }

    // A refusal starts nothing: the transform's run history is still empty.
    let history = ok(
        &router,
        "GetMLTaskRuns",
        &json!({ "TransformId": transform }),
    )
    .await;
    assert_eq!(
        history.get("TaskRuns").and_then(Value::as_array),
        Some(&vec![]),
        "a refused start must record no task run, got {history}"
    );

    // And the bucket an export was refused for holds nothing.
    let written = objects_under(&router, "exports/").await;
    assert!(
        written.is_empty(),
        "a refused export must write no object, got {written:?}"
    );

    // A path naming the bucket ALONE is where an export may write: the labels go UNDER the
    // path it names, so it is a prefix rather than an object.
    let export = start_export(&router, &transform, &format!("s3://{BUCKET}")).await;
    let listed = objects_under(&router, &format!("{export}.csv")).await;
    assert_eq!(
        listed,
        vec![format!("{export}.csv")],
        "an export to the bucket alone writes at the top of it, got {listed:?}"
    );
}
