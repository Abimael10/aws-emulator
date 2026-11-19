// SPDX-License-Identifier: CC0-1.0

//! A stack whose template declares an `AWS::CleanRoomsML::TrainingDataset` FROM THE START
//! deploys it: the submission is accepted, the dataset is really made in Clean Rooms ML's own
//! control plane, the stack publishes the ARN that service minted beside the status it reports
//! — and a declaration that service REFUSES fails the resource and rolls the create back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare on EVERY
//! operation that submits a template for deployment — `CreateStack` and `CreateChangeSet` as
//! much as `UpdateStack`, in either body form — and a declaration of it is deployed by calling
//! the service that OWNS the type, so what that service refuses is a FAILED RESOURCE rather
//! than a template-format error, while a name the reference does NOT publish is still refused
//! `Template format error: Unrecognized resource types: [<types>]`.** Four crates have to agree
//! for that to hold (CloudFormation submits the template and makes the call, Clean Rooms ML
//! serves it, and it resolves the Glue table and the Amazon S3 objects behind it), so the suite
//! lives here: a crate-local test can only ever see one side of it.
//!
//! The sibling suite `cloudformation_provisions_cleanroomsml_training_datasets` states the same
//! rule for the body an `UpdateStack` submits — this one states it for the FIRST deployment of a
//! stack, which is the other half of the rule's "every operation that submits a template":
//!
//! * the admission table differs along the OPERATION that submits the body (`CreateStack`,
//!   `CreateChangeSet`) and the FORM it is written in (JSON, YAML);
//! * the refusing half of admission differs along the near misses of the same namespace — a
//!   plural, a wrong-cased resource, a wrong-cased namespace and an invented resource — so
//!   admitting the published type is not admitting `AWS::CleanRoomsML::<anything>`;
//! * the deploy table differs along the operation the deployment is driven by (`CreateStack`,
//!   and the `CreateChangeSet` + `ExecuteChangeSet` pair a deploying tool makes), where the
//!   Glue table stores its data, whether the declaration states the catalog the table lives in,
//!   how many columns the schema carries and which `ColumnType` each one is, whether the
//!   declaration carries a `Description` and the type's own `Tags`, and which published return
//!   value the output reads (`Ref`, `Fn::GetAtt TrainingDatasetArn`);
//! * the refusal table differs along the CLASS of location Clean Rooms ML will not train from —
//!   one nothing was ever staged under, and one holding only a zero-sized object.
//!
//! Every expected value is computed here from what the row declares, from the Region each
//! request is signed for and from the account every ARN this emulator mints carries — never read
//! back out of the answer being checked.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::is_recognized;
use smithy_server_runtime::MultiServiceRouter;

// The CloudFormation wire, the stack waits and the account and Region every ARN carries, owned
// once by the suite about a completed stack's outputs.
use crate::cloudformation_completed_stack_output_values::{
    cloudformation, settled_status, Answered, ACCOUNT, CREATE_COMPLETE, POLL, SETTLE_DEADLINE,
};
// The Clean Rooms ML estate, and AWS's own wording for what it refuses, owned once by the
// sibling suite about the body an update submits.
use crate::cloudformation_provisions_cleanroomsml_training_datasets::{
    asserted_arn, asserted_sentence, build_router, failure_reasons, get_training_dataset,
    listed_training_datasets, no_records_sentence, quoted_and_class, s3, stage_object,
    training_data, ACTIVE, ARN_ATTRIBUTE, ARN_ATTRIBUTE_MEMBER, ARN_OUTPUT, DATABASE, DATASET,
    INVALID_REQUEST, RECORDS, REF_OUTPUT, ROLE_ARN, STATUS_ATTRIBUTE, STATUS_OUTPUT, TABLE,
    UNRECOGNIZED_TYPES,
};
use crate::query_wire::{opt_element, outputs_of};

/// The resource type this suite deploys, as the "AWS resource and property types reference"
/// publishes it under Clean Rooms ML's own `cloudFormationName`.
const TRAINING_DATASET: &str = "AWS::CleanRoomsML::TrainingDataset";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The error code that refusal carries, at HTTP 400.
const VALIDATION_ERROR: &str = "ValidationError";
const REFUSED_SUBMISSION: u16 = 400;

/// The terminal `StackStatus` of a create whose resource the owning service refused: "the
/// resources it made are removed again and the stack settles into ROLLBACK_COMPLETE".
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The statuses a change set settles on, and the one that offers it for execution.
const CHANGE_SET_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CHANGE_SET_FAILED: &str = "FAILED";
const AVAILABLE: &str = "AVAILABLE";

/// The name every change set below is created under.
const CHANGE_SET: &str = "training-dataset-plan";

/// The one `DatasetType` the model admits, which every declaration below states.
const INTERACTIONS: &str = "INTERACTIONS";

/// Where one row's Glue table stores its data, and what the bucket holds under it.
struct Stored {
    /// The key suffix the table's `StorageDescriptor$Location` names after the bucket, as the
    /// declaration spells it.
    suffix: &'static str,
    /// The objects the row stages in the bucket before the deployment: `(key, bytes)`. A row
    /// staging nothing leaves the location empty.
    staged: &'static [(&'static str, &'static [u8])],
}

/// Everything one row's template declares about its catalog, its table and the dataset over
/// them.
struct Declaration {
    /// The bucket the table's location names. It is created OUT of band, before the stack, so
    /// the objects a row stages are already in place when the dataset's create reads them.
    bucket: &'static str,
    database: &'static str,
    table: &'static str,
    /// The name the declaration gives the dataset, which that service holds it under.
    dataset: &'static str,
    stored: Stored,
    /// Whether the declaration states the catalog the table lives in, or leaves it to the
    /// service (which resolves it to the calling account's own).
    states_catalog: bool,
    description: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
    /// The schema the dataset declares: one `(column name, column type)` per column.
    schema: &'static [(&'static str, &'static str)],
}

/// Which operation a row's deployment is driven by — the axis the rule generalises over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Submit {
    /// `CreateStack`, which deploys what it is given straight away.
    Stack,
    /// `CreateChangeSet(ChangeSetType=CREATE)` then `ExecuteChangeSet`, the pair a deploying
    /// tool makes.
    ChangeSet,
}

/// One row of the deploy table.
struct Deployed {
    case: &'static str,
    stack: &'static str,
    submit: Submit,
    declaration: Declaration,
    /// Which output the row reads the dataset's ARN out of.
    arn_output: &'static str,
}

/// The column types below are `ColumnType`'s own values
/// (`smithy_query(query="shape",shape="ColumnType")`: `USER_ID`, `ITEM_ID`, `TIMESTAMP`,
/// `CATEGORICAL_FEATURE`, `NUMERICAL_FEATURE`).
const DEPLOYED: &[Deployed] = &[
    Deployed {
        case: "a CreateStack declaring the dataset over a nested prefix",
        stack: "cleanroomsml-created-dataset-prefix",
        submit: Submit::Stack,
        declaration: Declaration {
            bucket: "cleanroomsml-created-prefix-bucket",
            database: "cleanroomsml_created_prefix_db",
            table: "created_events",
            dataset: "created-prefix-training-dataset",
            stored: Stored {
                suffix: "data/",
                staged: &[("data/part-00000.csv", RECORDS)],
            },
            states_catalog: true,
            description: Some("the interactions the prefix holds"),
            tags: &[("Tier", "analytics")],
            schema: &[
                ("user_id", "USER_ID"),
                ("item_id", "ITEM_ID"),
                ("timestamp", "TIMESTAMP"),
            ],
        },
        arn_output: REF_OUTPUT,
    },
    Deployed {
        case: "an executed CREATE change set declaring the dataset over the bucket's root",
        stack: "cleanroomsml-created-dataset-change-set",
        submit: Submit::ChangeSet,
        declaration: Declaration {
            bucket: "cleanroomsml-created-root-bucket",
            database: "cleanroomsml_created_root_db",
            table: "root_events",
            dataset: "created-root-training-dataset",
            stored: Stored {
                suffix: "",
                staged: &[("part-00000.csv", RECORDS)],
            },
            states_catalog: false,
            description: None,
            tags: &[],
            schema: &[
                ("user_id", "USER_ID"),
                ("item_id", "ITEM_ID"),
                ("timestamp", "TIMESTAMP"),
                ("category", "CATEGORICAL_FEATURE"),
                ("weight", "NUMERICAL_FEATURE"),
            ],
        },
        arn_output: ARN_OUTPUT,
    },
];

/// One row of the refusal table: the same first deployment, differing only in what the table's
/// location holds.
struct Refused {
    case: &'static str,
    stack: &'static str,
    declaration: Declaration,
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "a prefix nothing was ever staged under",
        stack: "cleanroomsml-created-dataset-empty",
        declaration: Declaration {
            bucket: "cleanroomsml-created-empty-bucket",
            database: "cleanroomsml_created_empty_db",
            table: "empty_events",
            dataset: "created-empty-training-dataset",
            stored: Stored {
                suffix: "data/",
                staged: &[],
            },
            states_catalog: true,
            description: None,
            tags: &[],
            schema: &[("user_id", "USER_ID"), ("item_id", "ITEM_ID")],
        },
    },
    Refused {
        case: "a prefix holding only a zero-sized object",
        stack: "cleanroomsml-created-dataset-zero",
        declaration: Declaration {
            bucket: "cleanroomsml-created-zero-bucket",
            database: "cleanroomsml_created_zero_db",
            table: "zero_events",
            dataset: "created-zero-training-dataset",
            stored: Stored {
                suffix: "data/",
                staged: &[("data/part-00000.csv", b"")],
            },
            states_catalog: false,
            description: Some("a location with nothing to train on"),
            tags: &[("Tier", "research")],
            schema: &[("user_id", "USER_ID"), ("item_id", "ITEM_ID")],
        },
    },
];

/// The location a row's table stores its data at, which is also the location a refusal names:
/// the table's own stored value, character for character.
fn location(declaration: &Declaration) -> String {
    format!(
        "s3://{}/{}",
        declaration.bucket, declaration.stored.suffix
    )
}

/// Create the bucket the row's table is located in and stage the objects it holds, so the
/// dataset's create reads a location that is already in the state the row is about.
///
/// `PUT /<bucket>` is Amazon S3's `CreateBucket`, which needs no `CreateBucketConfiguration` in
/// the Region these suites are signed for.
async fn staged_bucket(router: &MultiServiceRouter, case: &str, declaration: &Declaration) {
    let created = s3(router, "PUT", &format!("/{}", declaration.bucket), b"").await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected Amazon S3 to create {:?}, got {} with {:?}",
        declaration.bucket,
        created.status,
        created.text()
    );
    for (key, body) in declaration.stored.staged {
        stage_object(router, case, declaration.bucket, key, body).await;
    }
}

/// The template a row submits: the Glue catalog and table the data lives behind, and the
/// training dataset declared over them in the SAME body, publishing both of the type's return
/// values and the status its owning service reports.
fn template(declaration: &Declaration) -> String {
    let mut properties = json!({
        "Name": declaration.dataset,
        "RoleArn": ROLE_ARN,
        "TrainingData": training_data(
            declaration.table,
            declaration.states_catalog,
            declaration.schema,
        ),
    });
    if let Some(description) = declaration.description {
        properties["Description"] = json!(description);
    }
    if !declaration.tags.is_empty() {
        properties["Tags"] = declaration
            .tags
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect();
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a Glue catalog and table, and the training dataset declared over them",
        "Resources": {
            DATABASE: {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": declaration.database } }
            },
            TABLE: {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE },
                    "TableInput": {
                        "Name": declaration.table,
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Location": location(declaration),
                            "Columns": declaration
                                .schema
                                .iter()
                                .map(|(name, _)| json!({ "Name": name, "Type": "string" }))
                                .collect::<Vec<Value>>()
                        }
                    }
                }
            },
            DATASET: {
                "Type": TRAINING_DATASET,
                "DependsOn": TABLE,
                "Properties": properties
            }
        },
        "Outputs": {
            REF_OUTPUT: { "Value": { "Ref": DATASET } },
            ARN_OUTPUT: { "Value": { "Fn::GetAtt": [DATASET, ARN_ATTRIBUTE] } },
            STATUS_OUTPUT: { "Value": { "Fn::GetAtt": [DATASET, STATUS_ATTRIBUTE] } },
        }
    })
    .to_string()
}

/// Hold one submission to the admission half of the rule: it is ACCEPTED, the answer carries
/// the identity of what it created, and the `Unrecognized resource types` refusal is nowhere in
/// it.
fn asserted_admission(case: &str, answered: &Answered, identity: &str) {
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 for a template declaring {TRAINING_DATASET} \u{2014} it \
         is a type a template may declare \u{2014} got {} with {body}",
        answered.status
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    assert!(
        opt_element(&body, identity).is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected the submission to answer its {identity}, got {body}"
    );
}

/// Poll `DescribeChangeSet` the way a deploying tool's waiter does, and answer the status the
/// creation SETTLED on, the execution status beside it and the answer they were read from.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let answered = cloudformation(
            router,
            "DescribeChangeSet",
            &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
        )
        .await;
        let body = answered.text();
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from DescribeChangeSet, got {} with {body}",
            answered.status
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        let execution = opt_element(&body, "ExecutionStatus").unwrap_or_default();
        if status == CHANGE_SET_CREATE_COMPLETE || status == CHANGE_SET_FAILED {
            return (status, execution, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the change set of {stack:?} to settle, last saw \
             {status:?}/{execution:?} in {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Deploy `template` as `stack` through `submit`, asserting every submission on the way was
/// ACCEPTED, and answer the settled status with the `DescribeStacks` body it was read from.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    submit: Submit,
    template: &str,
) -> (String, String) {
    match submit {
        Submit::Stack => {
            let created = cloudformation(
                router,
                "CreateStack",
                &[("StackName", stack), ("TemplateBody", template)],
            )
            .await;
            asserted_admission(case, &created, "StackId");
        }
        Submit::ChangeSet => {
            let planned = cloudformation(
                router,
                "CreateChangeSet",
                &[
                    ("StackName", stack),
                    ("ChangeSetName", CHANGE_SET),
                    ("ChangeSetType", "CREATE"),
                    ("TemplateBody", template),
                ],
            )
            .await;
            asserted_admission(case, &planned, "Id");
            let (status, execution, body) = settled_change_set(router, case, stack).await;
            assert_eq!(
                (status.as_str(), execution.as_str()),
                (CHANGE_SET_CREATE_COMPLETE, AVAILABLE),
                "case {case:?}: expected the plan over a published type to be offered for \
                 execution, got {body}"
            );
            let executed = cloudformation(
                router,
                "ExecuteChangeSet",
                &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
            )
            .await;
            assert_eq!(
                executed.status,
                200,
                "case {case:?}: expected HTTP 200 from ExecuteChangeSet, got {} with {:?}",
                executed.status,
                executed.text()
            );
        }
    }
    settled_status(router, stack).await
}

/// Every resource failure the stack recorded, which is what a row that did not reach the status
/// it must quotes.
async fn stack_failures(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    failure_reasons(router, case, stack, DATASET).await
}

#[tokio::test]
async fn every_operation_that_submits_a_template_admits_the_training_dataset_type() {
    let router = build_router();
    assert!(
        is_recognized(TRAINING_DATASET),
        "case {TRAINING_DATASET:?}: expected a published type to be recognized, it is not"
    );

    // The body in each form: the type and nothing else, so what is under test is the ADMISSION
    // of the declaration rather than anything a deployment of it goes on to do.
    for (form, body) in [
        (
            "JSON",
            json!({ "Resources": { DATASET: { "Type": TRAINING_DATASET } } }).to_string(),
        ),
        (
            "YAML",
            [
                "Resources:",
                &format!("  {DATASET}:"),
                &format!("    Type: {TRAINING_DATASET}"),
                "",
            ]
            .join("\n"),
        ),
    ] {
        let stack = format!("cleanroomsml-admitted-{}", form.to_ascii_lowercase());

        let case = format!("{TRAINING_DATASET} submitted to CreateStack in {form}");
        let created = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &body)],
        )
        .await;
        asserted_admission(&case, &created, "StackId");
        assert!(
            opt_element(&created.text(), "StackId")
                .is_some_and(|id| id.contains(&format!(":stack/{stack}/"))),
            "case {case:?}: expected the id of the stack the submission created, got {}",
            created.text()
        );
        // The stack really is there: the reported defect created none at all.
        let described =
            cloudformation(&router, "DescribeStacks", &[("StackName", &stack)]).await;
        assert_eq!(
            described.status,
            200,
            "case {case:?}: expected DescribeStacks to report the created stack, got {} with {:?}",
            described.status,
            described.text()
        );

        let case = format!("{TRAINING_DATASET} submitted to CreateChangeSet in {form}");
        let planned = cloudformation(
            &router,
            "CreateChangeSet",
            &[
                ("StackName", &format!("{stack}-change-set")),
                ("ChangeSetName", CHANGE_SET),
                ("ChangeSetType", "CREATE"),
                ("TemplateBody", &body),
            ],
        )
        .await;
        asserted_admission(&case, &planned, "Id");
    }
}

#[tokio::test]
async fn a_name_the_reference_does_not_publish_is_still_refused() {
    let router = build_router();
    // The near misses of the published type: a plural, a wrong-cased resource, a wrong-cased
    // namespace and an invented resource. Admitting the published type is not admitting
    // `AWS::CleanRoomsML::<anything>`.
    let unpublished = [
        "AWS::CleanRoomsML::TrainingDatasets",
        "AWS::CleanRoomsML::Trainingdataset",
        "AWS::CleanroomsML::TrainingDataset",
        "AWS::CleanRoomsML::TrainingDataSource",
    ];
    for (index, resource_type) in unpublished.into_iter().enumerate() {
        let case = format!("{resource_type} declared");
        assert!(
            !is_recognized(resource_type),
            "case {case:?}: expected a type the reference does not publish to be unrecognized, \
             it is not"
        );
        let body = json!({ "Resources": { DATASET: { "Type": resource_type } } }).to_string();
        let refused = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &format!("cleanroomsml-unpublished-{index}")),
                ("TemplateBody", &body),
            ],
        )
        .await;
        let answer = refused.text();
        assert_eq!(
            refused.status, REFUSED_SUBMISSION,
            "case {case:?}: expected HTTP {REFUSED_SUBMISSION} for a type the reference does not \
             publish, got {} with {answer}",
            refused.status
        );
        assert!(
            answer.contains(VALIDATION_ERROR)
                && answer.contains(&format!("{UNRECOGNIZED_PREFIX}{resource_type}]")),
            "case {case:?}: expected the unrecognized-type refusal naming the declared type, got \
             {answer}"
        );
    }
}

#[tokio::test]
async fn a_first_deployment_declaring_a_training_dataset_records_it_in_clean_rooms_ml() {
    for row in DEPLOYED {
        // One emulator per row: each row deploys its own catalog and dataset, and a fresh engine
        // is what keeps one row's listing from reporting another's dataset.
        let router = build_router();
        let case = row.case;
        let declared = &row.declaration;

        staged_bucket(&router, case, declared).await;

        // 1. The submission is ACCEPTED and the deployment completes: the type is one the
        // registry knows, and the resource is created in the service that owns it.
        let (settled, described) =
            deployed(&router, case, row.stack, row.submit, &template(declared)).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the first deployment to complete, got {settled:?}; the \
             failures it reported: {:?}; last answer {described}",
            stack_failures(&router, case, row.stack).await
        );

        // 2. BOTH published return values resolve to the ARN Clean Rooms ML minted: "Ref returns
        // the training dataset ARN", and `Fn::GetAtt TrainingDatasetArn` reads the same value.
        let outputs = outputs_of(&described);
        let published = outputs.get(row.arn_output).cloned().unwrap_or_default();
        let arn = asserted_arn(case, &published);
        assert_eq!(
            outputs.get(REF_OUTPUT).cloned().unwrap_or_default(),
            arn,
            "case {case:?}: expected Ref to return the training dataset ARN, got {outputs:?}"
        );
        assert_eq!(
            outputs.get(ARN_OUTPUT).cloned().unwrap_or_default(),
            arn,
            "case {case:?}: expected Fn::GetAtt {ARN_ATTRIBUTE} to read the same ARN, got \
             {outputs:?}"
        );
        // The status is the one Clean Rooms ML reports for the dataset, not one CloudFormation
        // decided, and a published return value with a key and nothing under it is a map a
        // client cannot build.
        assert_eq!(
            outputs.get(STATUS_OUTPUT).map(String::as_str),
            Some(ACTIVE),
            "case {case:?}: expected Fn::GetAtt {STATUS_ATTRIBUTE} to read the status the owning \
             service reports, got {outputs:?}"
        );

        // 3. The dataset really is in Clean Rooms ML, under everything the template declared.
        let read = get_training_dataset(&router, &arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected Clean Rooms ML to hold the dataset the stack reported as \
             created, got {} with {:?}",
            read.status,
            read.text()
        );
        let reported = read.json();
        for (member, expected) in [
            (ARN_ATTRIBUTE_MEMBER, arn.clone()),
            ("name", declared.dataset.to_string()),
            ("roleArn", ROLE_ARN.to_string()),
            ("status", ACTIVE.to_string()),
        ] {
            assert_eq!(
                reported.get(member).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected {member:?} to be {expected:?}, got {reported}"
            );
        }
        assert_eq!(
            reported.get("description").and_then(Value::as_str),
            declared.description,
            "case {case:?}: expected the declared description, got {reported}"
        );
        let dataset = reported
            .pointer("/trainingData/0")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            dataset.get("type").and_then(Value::as_str),
            Some(INTERACTIONS),
            "case {case:?}: expected the declared dataset type, got {dataset}"
        );
        let glue = dataset
            .pointer("/inputConfig/dataSource/glueDataSource")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            (
                glue.get("databaseName").and_then(Value::as_str),
                glue.get("tableName").and_then(Value::as_str),
                glue.get("catalogId").and_then(Value::as_str),
            ),
            (Some(declared.database), Some(declared.table), Some(ACCOUNT)),
            "case {case:?}: expected the Glue table the template named, with the catalog \
             resolved to this account, got {glue}"
        );
        let mut columns: Vec<(String, String)> = dataset
            .pointer("/inputConfig/schema")
            .and_then(Value::as_array)
            .map(|schema| {
                schema
                    .iter()
                    .map(|column| {
                        (
                            column
                                .get("columnName")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            column
                                .pointer("/columnTypes/0")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        columns.sort();
        // Compared as a SET: what this rule is about is that every column the template declared
        // reaches the service that owns the dataset, with the type it was declared under. The
        // ORDER that service reports them in is its own, and is pinned by its own crate's suite.
        let mut declared_columns: Vec<(String, String)> = declared
            .schema
            .iter()
            .map(|(name, kind)| ((*name).to_string(), (*kind).to_string()))
            .collect();
        declared_columns.sort();
        assert_eq!(
            columns, declared_columns,
            "case {case:?}: expected the declared schema, got {dataset}"
        );
        assert_eq!(
            listed_training_datasets(&router, case).await,
            vec![arn.clone()],
            "case {case:?}: expected the dataset the stack created to be the one this account \
             lists"
        );
    }
}

#[tokio::test]
async fn a_first_deployment_clean_rooms_ml_refuses_rolls_back() {
    for row in REFUSED {
        let router = build_router();
        let case = row.case;
        let declared = &row.declaration;

        staged_bucket(&router, case, declared).await;

        // 1. The create is ACCEPTED — the type is recognised — and then rolled back, because the
        // resource the owning service refused is a failed resource rather than a bad template.
        let (settled, described) = deployed(
            &router,
            case,
            row.stack,
            Submit::Stack,
            &template(declared),
        )
        .await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the create to roll back for a location Clean Rooms ML will \
             not train from, got {settled:?}; the failures it reported: {:?}; last answer \
             {described}",
            stack_failures(&router, case, row.stack).await
        );
        assert!(
            outputs_of(&described).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // 2. ONE failed event for the dataset, whose reason quotes Clean Rooms ML's own
        // exception, names the location its Glue table states, and is classified as a refusal of
        // the request.
        let reasons = stack_failures(&router, case, row.stack).await;
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: expected exactly one failed event for {DATASET}, got {reasons:?}"
        );
        let (quoted, class) = quoted_and_class(case, &reasons[0]);
        assert_eq!(
            asserted_sentence(case, &quoted),
            no_records_sentence(&location(declared)),
            "case {case:?}: expected the sentence Clean Rooms ML words for a location holding no \
             data, got {:?}",
            reasons[0]
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the class a refusal of the request falls in, got {class:?} \
             in {:?}",
            reasons[0]
        );

        // 3. And no dataset of that name exists: a resource whose create was refused exists in
        // no service.
        assert!(
            listed_training_datasets(&router, case).await.is_empty(),
            "case {case:?}: expected Clean Rooms ML to hold no training dataset at all"
        );
    }
}
