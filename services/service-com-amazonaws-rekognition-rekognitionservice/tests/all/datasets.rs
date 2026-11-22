// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of Rekognition's Custom Labels DATASET lifecycle over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.<Operation>`), dispatched
//! through the same [`MultiServiceRouter`] the production emulator mounts.
//!
//! Every assertion goes over HTTP and reads only the status, the wire error code and the JSON
//! body: nothing reaches into the service's state struct, so a response that is not computed
//! from persisted state cannot pass.
//!
//! THE RULE UNDER TEST: `CreateDataset` records ONE dataset per half of a project's data —
//! "to create a training dataset for a project, specify `TRAIN` for the value of
//! `DatasetType`. To create the test dataset for a project, specify `TEST`" — answers the
//! `DatasetArn` this service minted for it, and refuses a second create of a half the project
//! already holds; what the dataset HOLDS is counted off the source the create named, never
//! assumed; `DescribeDataset` reports that record, with the status the clock owes it
//! ("creating a dataset takes a while to complete … the dataset created successfully if the
//! value of `Status` is `CREATE_COMPLETE`"); the owning project's own `DescribeProjects`
//! summarises every dataset it holds; and `DeleteDataset` starts the removal of the dataset its
//! ARN names, after which "if you try to access the dataset after it is deleted, you get a
//! `ResourceNotFoundException` exception".
//!
//! Each test asserts that rule over a TABLE whose rows differ along what it generalises over:
//! which half of the project's data the dataset is (both values the modelled `DatasetType`
//! admits), the form of the owning project's name, the Region the call is signed in — which is
//! where the ARN's Region comes from — whether the create attaches tags, the SOURCE the
//! dataset is created from (none, a Ground Truth manifest, a copy of another dataset) and the
//! content of that manifest (entries with labels, entries without, an entry that is not an
//! entry at all, none).
//!
//! Every wait here is a poll for the CONDITION under a generous deadline: the move from
//! `CREATE_IN_PROGRESS` to `CREATE_COMPLETE` and a deleted dataset's disappearance are
//! transitions the clock owes, and nothing here asserts how long one took.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::face_collections::{call, call_in, harness, Reply};
use crate::face_detection::{harness_with_object_store, s3};

/// The Region the single-Region rows are signed in, and the account id every ARN this
/// emulator mints carries (`engine::mock_account_id`).
const REGION: &str = "us-east-1";
const ACCOUNT: &str = "000000000000";

/// The two values the modelled `DatasetType` admits
/// (`smithy_query(query="shape",shape="DatasetType")`: `[TRAIN, TEST]`).
const TRAIN: &str = "TRAIN";
const TEST: &str = "TEST";

/// The `DatasetStatus` values this suite reads (modelled `DatasetStatus`).
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DatasetStatusMessageCode` a dataset operation that went through reports (modelled
/// `DatasetStatusMessageCode`: `[SUCCESS, SERVICE_ERROR, CLIENT_ERROR]`).
const SUCCESS: &str = "SUCCESS";

/// How long one poll waits before looking again, and how long the whole wait may take.
///
/// The deadline is a ceiling on the WAIT and nothing else: the creation span is this service's
/// own, so the loops look again until the status holds, and the deadline is generous enough
/// that a loaded host running the whole workspace's tests at once still gets there.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

/// Create one project and answer the `ProjectArn` this service minted for it.
async fn project(router: &MultiServiceRouter, region: &str, project_name: &str) -> String {
    let reply = call_in(
        router,
        region,
        "CreateProject",
        json!({ "ProjectName": project_name }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "CreateProject on {project_name:?} must answer 200, got {:?}",
        reply.body
    );
    reply
        .string("ProjectArn")
        .unwrap_or_else(|| panic!("CreateProject answered no ProjectArn: {:?}", reply.body))
}

/// The prefix the ARN of a `dataset_type` dataset of `project_name` in `region` carries, which
/// the modelled `DatasetArn` pattern fixes:
/// `arn:aws:rekognition:<region>:<account>:project/<name>/dataset/(train|test)/`.
fn dataset_arn_prefix(region: &str, project_name: &str, dataset_type: &str) -> String {
    format!(
        "arn:aws:rekognition:{region}:{ACCOUNT}:project/{project_name}/dataset/{}/",
        dataset_type.to_ascii_lowercase()
    )
}

/// Assert `arn` has the form the modelled `DatasetArn` pattern fixes for this project and this
/// half of its data: the prefix above plus a non-empty run of digits, which is the version
/// segment the pattern ends in (`…/[0-9]+$`).
fn assert_dataset_arn_form(case: &str, arn: &str, region: &str, project_name: &str, dataset_type: &str) {
    let prefix = dataset_arn_prefix(region, project_name, dataset_type);
    let version = arn.strip_prefix(&prefix).unwrap_or_default();
    assert!(
        !version.is_empty() && version.chars().all(|character| character.is_ascii_digit()),
        "case {case:?}: expected a DatasetArn of the form {prefix}<version>, got {arn:?}"
    );
}

/// The `DatasetDescription` one `DescribeDataset` reports, asserting the read was served.
async fn described(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    dataset_arn: &str,
) -> Value {
    let reply = call_in(
        router,
        region,
        "DescribeDataset",
        json!({ "DatasetArn": dataset_arn }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: DescribeDataset on {dataset_arn:?} must answer 200, got {:?}",
        reply.body
    );
    reply
        .field("DatasetDescription")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: DescribeDataset answered no DatasetDescription: {:?}",
                reply.body
            )
        })
}

/// Poll `DescribeDataset` until the dataset reports `CREATE_COMPLETE`, and answer the
/// description it reported then; the last description read is answered when the deadline
/// passes, so a dataset that never completes fails on the status it was stuck at.
async fn completed(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    dataset_arn: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let description = described(router, region, case, dataset_arn).await;
        let status = text_of(&description, "Status");
        if status == CREATE_COMPLETE || Instant::now() >= until {
            return description;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `DescribeDataset` until the dataset is one this service no longer holds, and answer
/// the last reply: a dataset whose deletion never completes fails on what it still answered.
async fn until_gone(
    router: &MultiServiceRouter,
    region: &str,
    dataset_arn: &str,
) -> Reply {
    let until = Instant::now() + DEADLINE;
    loop {
        let reply = call_in(
            router,
            region,
            "DescribeDataset",
            json!({ "DatasetArn": dataset_arn }),
        )
        .await;
        if reply.status != 200 || Instant::now() >= until {
            return reply;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The text one reported member carries, or the empty string where the report carries none.
fn text_of(reported: &Value, member: &str) -> String {
    reported
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The number one reported member carries, or `None` where the report carries none.
fn number_of(reported: &Value, member: &str) -> Option<i64> {
    reported.get(member).and_then(Value::as_i64)
}

/// The datasets the owning project's own `DescribeProjects` summarises, each as
/// `(DatasetArn, DatasetType, Status)`.
async fn summarised(
    router: &MultiServiceRouter,
    region: &str,
    project_name: &str,
) -> Vec<(String, String, String)> {
    let reply = call_in(
        router,
        region,
        "DescribeProjects",
        json!({ "ProjectNames": [project_name] }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "DescribeProjects on {project_name:?} must answer 200, got {:?}",
        reply.body
    );
    reply
        .list("ProjectDescriptions")
        .first()
        .and_then(|description| description.get("Datasets").cloned())
        .and_then(|datasets| datasets.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|dataset| {
            (
                text_of(dataset, "DatasetArn"),
                text_of(dataset, "DatasetType"),
                text_of(dataset, "Status"),
            )
        })
        .collect()
}

/// One row of the dataset-lifecycle table.
struct Row {
    case: &'static str,
    /// The project the dataset belongs to, whose name's form the rows span.
    project_name: &'static str,
    /// Which half of that project's data the dataset is.
    dataset_type: &'static str,
    /// The tags the create attaches, if any.
    tags: Vec<(&'static str, &'static str)>,
    /// The Region the calls are signed in, which is where the ARNs' Region comes from.
    region: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a training dataset, no tags",
            project_name: "labels-train",
            dataset_type: TRAIN,
            tags: Vec::new(),
            region: REGION,
        },
        Row {
            case: "a test dataset with tags, in another Region",
            project_name: "labels_test.01",
            dataset_type: TEST,
            tags: vec![("team", "vision"), ("stage", "qa")],
            region: "eu-west-1",
        },
        Row {
            case: "a training dataset of a project named with every admitted character class",
            project_name: "a.b_c-D9",
            dataset_type: TRAIN,
            tags: vec![("only", "one")],
            region: "ap-southeast-2",
        },
    ]
}

#[tokio::test]
async fn a_created_dataset_reads_back_and_is_summarised_on_its_project() {
    for row in rows() {
        let router = harness();
        let case = row.case;
        let project_arn = project(&router, row.region, row.project_name).await;

        let mut request = json!({
            "ProjectArn": project_arn,
            "DatasetType": row.dataset_type,
        });
        if !row.tags.is_empty() {
            request["Tags"] = Value::Object(
                row.tags
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), json!(value)))
                    .collect(),
            );
        }
        let reply = call_in(&router, row.region, "CreateDataset", request.clone()).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: CreateDataset must answer 200, got {:?}",
            reply.body
        );
        let dataset_arn = reply
            .string("DatasetArn")
            .unwrap_or_else(|| panic!("case {case:?}: no DatasetArn in {:?}", reply.body));
        assert_dataset_arn_form(
            case,
            &dataset_arn,
            row.region,
            row.project_name,
            row.dataset_type,
        );

        // The clock owes the move out of the transient status the create's own documentation
        // tells a caller to poll for, so the read is repeated until it holds.
        let description = completed(&router, row.region, case, &dataset_arn).await;
        assert_eq!(
            text_of(&description, "Status"),
            CREATE_COMPLETE,
            "case {case:?}: expected the dataset's creation to complete, got {description}"
        );
        assert_eq!(
            text_of(&description, "StatusMessageCode"),
            SUCCESS,
            "case {case:?}: expected a created dataset to report the success message code, got \
             {description}"
        );
        // A dataset created with no source holds nothing, so every count it reports is zero
        // rather than a number nothing counted.
        let stats = description
            .get("DatasetStats")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no DatasetStats in {description}"));
        for member in ["TotalEntries", "LabeledEntries", "TotalLabels", "ErrorEntries"] {
            assert_eq!(
                number_of(&stats, member),
                Some(0),
                "case {case:?}: expected {member} of a dataset with no source to be 0, got \
                 {stats}"
            );
        }
        assert!(
            description.get("CreationTimestamp").is_some()
                && description.get("LastUpdatedTimestamp").is_some(),
            "case {case:?}: expected both instants to be reported, got {description}"
        );

        // The owning project summarises exactly the dataset it holds.
        let listed = summarised(&router, row.region, row.project_name).await;
        assert_eq!(
            listed,
            vec![(
                dataset_arn.clone(),
                row.dataset_type.to_string(),
                CREATE_COMPLETE.to_string()
            )],
            "case {case:?}: expected the project to summarise only its own dataset, got {listed:?}"
        );

        // A project holds at most ONE dataset of each half of its data, so the same create
        // again is a collision and the dataset that holds the type is left exactly as it was.
        let again = call_in(&router, row.region, "CreateDataset", request).await;
        assert_eq!(
            (again.status, again.error_code().as_str()),
            (400, "ResourceAlreadyExistsException"),
            "case {case:?}: expected a second dataset of the same type to be refused, got {:?}",
            again.body
        );
        let listed = summarised(&router, row.region, row.project_name).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the refused create to leave one dataset, got {listed:?}"
        );

        // Deleting it takes it out of the project, after which nothing resolves its ARN.
        let deleted = call_in(
            &router,
            row.region,
            "DeleteDataset",
            json!({ "DatasetArn": dataset_arn }),
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteDataset must answer 200, got {:?}",
            deleted.body
        );
        let gone = until_gone(&router, row.region, &dataset_arn).await;
        assert_eq!(
            (gone.status, gone.error_code().as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected a deleted dataset to be refused as not found, got {:?}",
            gone.body
        );
        assert_eq!(
            gone.string("Message").unwrap_or_default(),
            format!("The dataset {dataset_arn} doesn't exist. Check that the dataset exists."),
            "case {case:?}: expected the refusal AWS words for a dataset that is not there, got \
             {:?}",
            gone.body
        );
        let listed = summarised(&router, row.region, row.project_name).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the project to hold no dataset after the delete, got \
             {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_project_holds_one_dataset_of_each_half_of_its_data() {
    let router = harness();
    let project_arn = project(&router, REGION, "labels-both").await;
    let mut arns = Vec::new();
    for dataset_type in [TRAIN, TEST] {
        let reply = call(
            &router,
            "CreateDataset",
            json!({ "ProjectArn": project_arn, "DatasetType": dataset_type }),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {dataset_type:?}: CreateDataset must answer 200, got {:?}",
            reply.body
        );
        let arn = reply
            .string("DatasetArn")
            .unwrap_or_else(|| panic!("case {dataset_type:?}: no DatasetArn in {:?}", reply.body));
        assert_dataset_arn_form(dataset_type, &arn, REGION, "labels-both", dataset_type);
        arns.push((dataset_type.to_string(), arn));
    }
    for (dataset_type, arn) in &arns {
        completed(&router, REGION, dataset_type, arn).await;
    }
    let listed: BTreeSet<(String, String)> = summarised(&router, REGION, "labels-both")
        .await
        .into_iter()
        .map(|(arn, dataset_type, _)| (dataset_type, arn))
        .collect();
    let expected: BTreeSet<(String, String)> = arns.iter().cloned().collect();
    assert_eq!(
        listed, expected,
        "expected the project to summarise both halves of its data, got {listed:?}"
    );
}

#[tokio::test]
async fn a_dataset_names_a_project_this_account_has_and_a_dataset_this_account_has() {
    let router = harness();
    let project_arn = project(&router, REGION, "labels-refusals").await;
    let absent_project = format!("arn:aws:rekognition:{REGION}:{ACCOUNT}:project/nothing/1");
    let absent_dataset =
        format!("arn:aws:rekognition:{REGION}:{ACCOUNT}:project/nothing/dataset/train/1");

    // (case, the request, the expected code, the expected message).
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "a ProjectArn naming no project of this account",
            json!({ "ProjectArn": absent_project, "DatasetType": TRAIN }),
            "ResourceNotFoundException",
            format!("Project {absent_project} does not exist"),
        ),
        (
            "a DatasetSource stating both of its members",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TRAIN,
                "DatasetSource": {
                    "DatasetArn": absent_dataset,
                    "GroundTruthManifest": {"S3Object": {"Bucket": "manifests", "Name": "k"}}
                }
            }),
            "InvalidParameterException",
            "Request has invalid parameters".to_string(),
        ),
        (
            "a DatasetSource stating neither of its members",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TRAIN,
                "DatasetSource": {}
            }),
            "InvalidParameterException",
            "Request has invalid parameters".to_string(),
        ),
        (
            "a DatasetSource copying a dataset this account does not hold",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TRAIN,
                "DatasetSource": {"DatasetArn": absent_dataset}
            }),
            "ResourceNotFoundException",
            format!("The dataset {absent_dataset} doesn't exist. Check that the dataset exists."),
        ),
    ];
    for (case, request, code, message) in rows {
        let reply = call(&router, "CreateDataset", request).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, code.to_string()),
            "case {case:?}: expected the create to be refused {code}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Message").unwrap_or_default(),
            message,
            "case {case:?}: expected the refusal's own message, got {:?}",
            reply.body
        );
    }

    // And nothing was recorded for any of them: the project still holds no dataset.
    let listed = summarised(&router, REGION, "labels-refusals").await;
    assert!(
        listed.is_empty(),
        "expected every refused create to record nothing, got {listed:?}"
    );

    // A DatasetArn naming nothing is refused by the reads on a dataset too.
    for operation in ["DescribeDataset", "DeleteDataset"] {
        let reply = call(&router, operation, json!({ "DatasetArn": absent_dataset })).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "ResourceNotFoundException".to_string()),
            "case {operation:?}: expected a dataset ARN naming nothing to be refused, got {:?}",
            reply.body
        );
    }
}

/// One Ground Truth manifest row: the JSON Lines the object holds, and the counts the dataset
/// created from it must report.
struct ManifestRow {
    case: &'static str,
    key: &'static str,
    /// The object's content, as the lines a manifest is written in.
    body: &'static str,
    total_entries: i64,
    labeled_entries: i64,
    total_labels: i64,
    error_entries: i64,
}

/// The manifest rows, each counted off the content beside it.
///
/// The format is the Amazon SageMaker Ground Truth one a dataset is created from: one JSON
/// Line per image, carrying the `source-ref` that names it and, for a labeled entry, a label
/// attribute whose own `<attribute>-metadata` names the label — `class-name` for an
/// image-level label, `class-map` for the classes of a localization attribute
/// (`smithy_query(query="shape",shape="DatasetChanges")` describes the entry form).
fn manifest_rows() -> Vec<ManifestRow> {
    vec![
        ManifestRow {
            case: "two image-level labels over three labeled entries",
            key: "labels.manifest",
            body: "\
{\"source-ref\":\"s3://pics/one.png\",\"kind\":0,\"kind-metadata\":{\"class-name\":\"cat\"}}
{\"source-ref\":\"s3://pics/two.png\",\"kind\":1,\"kind-metadata\":{\"class-name\":\"dog\"}}
{\"source-ref\":\"s3://pics/three.png\",\"kind\":0,\"kind-metadata\":{\"class-name\":\"cat\"}}
",
            total_entries: 3,
            labeled_entries: 3,
            total_labels: 2,
            error_entries: 0,
        },
        ManifestRow {
            case: "a localization class map beside an unlabeled entry",
            key: "boxes.manifest",
            body: "\
{\"source-ref\":\"s3://pics/four.png\",\"boxes\":{\"annotations\":[{\"class_id\":0}]},\"boxes-metadata\":{\"class-map\":{\"0\":\"car\",\"1\":\"van\"}}}
{\"source-ref\":\"s3://pics/five.png\"}
",
            total_entries: 2,
            labeled_entries: 1,
            total_labels: 2,
            error_entries: 0,
        },
        ManifestRow {
            case: "a line that is not an entry and one that names no image",
            key: "broken.manifest",
            body: "\
{\"source-ref\":\"s3://pics/six.png\",\"kind\":0,\"kind-metadata\":{\"class-name\":\"bus\"}}
not json at all
{\"kind\":0}
",
            total_entries: 3,
            labeled_entries: 1,
            total_labels: 1,
            error_entries: 2,
        },
        ManifestRow {
            case: "an empty manifest",
            key: "empty.manifest",
            body: "",
            total_entries: 0,
            labeled_entries: 0,
            total_labels: 0,
            error_entries: 0,
        },
    ]
}

#[tokio::test]
async fn a_dataset_created_from_a_manifest_reports_the_entries_that_manifest_declares() {
    let router = harness_with_object_store();
    assert_eq!(
        s3(&router, "/manifests", &[]).await,
        200,
        "the manifest bucket must be created"
    );
    for row in manifest_rows() {
        let case = row.case;
        assert_eq!(
            s3(&router, &format!("/manifests/{}", row.key), row.body.as_bytes()).await,
            200,
            "case {case:?}: the manifest object must be staged"
        );
        let project_name = format!("manifest-{}", row.key.replace('.', "-"));
        let project_arn = project(&router, REGION, &project_name).await;
        let reply = call(
            &router,
            "CreateDataset",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TRAIN,
                "DatasetSource": {
                    "GroundTruthManifest": {
                        "S3Object": {"Bucket": "manifests", "Name": row.key}
                    }
                }
            }),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: CreateDataset from a staged manifest must answer 200, got {:?}",
            reply.body
        );
        let dataset_arn = reply
            .string("DatasetArn")
            .unwrap_or_else(|| panic!("case {case:?}: no DatasetArn in {:?}", reply.body));
        let description = described(&router, REGION, case, &dataset_arn).await;
        let stats = description
            .get("DatasetStats")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no DatasetStats in {description}"));
        for (member, expected) in [
            ("TotalEntries", row.total_entries),
            ("LabeledEntries", row.labeled_entries),
            ("TotalLabels", row.total_labels),
            ("ErrorEntries", row.error_entries),
        ] {
            assert_eq!(
                number_of(&stats, member),
                Some(expected),
                "case {case:?}: expected {member} to be the manifest's own count {expected}, got \
                 {stats}"
            );
        }

        // A COPY of that dataset holds what it holds, so it reports the same counts.
        let copy = call(
            &router,
            "CreateDataset",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TEST,
                "DatasetSource": {"DatasetArn": dataset_arn}
            }),
        )
        .await;
        assert_eq!(
            copy.status, 200,
            "case {case:?}: a copy of a dataset must answer 200, got {:?}",
            copy.body
        );
        let copied_arn = copy
            .string("DatasetArn")
            .unwrap_or_else(|| panic!("case {case:?}: no DatasetArn in {:?}", copy.body));
        let copied = described(&router, REGION, case, &copied_arn).await;
        assert_eq!(
            copied.get("DatasetStats"),
            Some(&stats),
            "case {case:?}: expected the copy to hold what the dataset it copied holds, got \
             {copied}"
        );
    }
}

#[tokio::test]
async fn a_manifest_the_object_store_does_not_serve_is_refused() {
    let router = harness_with_object_store();
    let project_arn = project(&router, REGION, "manifest-absent").await;
    // (case, the object the source names).
    let rows: Vec<(&str, Value)> = vec![
        (
            "a bucket the store does not serve",
            json!({"Bucket": "nowhere", "Name": "labels.manifest"}),
        ),
        (
            "an object the store holds nothing at",
            json!({"Bucket": "manifests", "Name": "missing.manifest"}),
        ),
        ("an object named incompletely", json!({"Bucket": "manifests"})),
    ];
    assert_eq!(
        s3(&router, "/manifests", &[]).await,
        200,
        "the manifest bucket must be created"
    );
    for (case, object) in rows {
        let reply = call(
            &router,
            "CreateDataset",
            json!({
                "ProjectArn": project_arn,
                "DatasetType": TRAIN,
                "DatasetSource": {"GroundTruthManifest": {"S3Object": object}}
            }),
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "InvalidS3ObjectException".to_string()),
            "case {case:?}: expected a manifest the store does not serve to be refused, got {:?}",
            reply.body
        );
    }
    // Nothing was recorded for any of them.
    let listed = summarised(&router, REGION, "manifest-absent").await;
    assert!(
        listed.is_empty(),
        "expected every refused create to record nothing, got {listed:?}"
    );
}

#[tokio::test]
async fn a_dataset_is_reported_creating_before_the_clock_completes_it() {
    let router = harness();
    let project_arn = project(&router, REGION, "labels-transient").await;
    let reply = call(
        &router,
        "CreateDataset",
        json!({ "ProjectArn": project_arn, "DatasetType": TRAIN }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "CreateDataset must answer 200, got {:?}",
        reply.body
    );
    let dataset_arn = reply
        .string("DatasetArn")
        .unwrap_or_else(|| panic!("no DatasetArn in {:?}", reply.body));

    // The statuses a dataset passes through, in the order a caller sees them: the one the
    // create's documentation tells a caller to poll for, then the one its completion is
    // reported in. Which of the two the FIRST read sees depends on the clock alone, so the
    // assertion is that every read reports one of them and that the last one completes —
    // never on how long the move took.
    let seen = text_of(
        &described(&router, REGION, "the first read", &dataset_arn).await,
        "Status",
    );
    assert!(
        seen == CREATE_IN_PROGRESS || seen == CREATE_COMPLETE,
        "expected a dataset to be reported creating or created, got {seen:?}"
    );
    let completed_description = completed(&router, REGION, "the settled read", &dataset_arn).await;
    assert_eq!(
        text_of(&completed_description, "Status"),
        CREATE_COMPLETE,
        "expected the dataset's creation to complete, got {completed_description}"
    );
}
