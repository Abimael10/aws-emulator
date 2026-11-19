// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` that adds an `AWS::CleanRoomsML::TrainingDataset` to a deployed stack
//! deploys it: the dataset is recorded in Clean Rooms ML's own control plane, the stack
//! publishes the ARN that service minted, deleting the stack takes the dataset away — and a
//! declaration that service REFUSES fails the resource with its own refusal and rolls the
//! update back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare on every
//! operation that submits a template for deployment, and a declaration of it is deployed by
//! calling the service that OWNS the type — so what the stack publishes is what that service
//! answered, and what that service refuses is a FAILED RESOURCE rather than a template-format
//! error.** Four crates have to agree for that to hold (CloudFormation submits the template and
//! makes the call, Clean Rooms ML serves it, and it resolves the Glue table and the Amazon S3
//! objects behind it), so the suite lives here: a crate-local test can only ever see one side of
//! it.
//!
//! The rows of the deploy table differ along what the rule generalises over: where the Glue
//! table stores its data (a nested prefix, a key the location names outright, the bucket's
//! root), whether the declaration states the catalog the table lives in or leaves it to the
//! service, how many columns the schema carries and which `ColumnType` each one is, whether the
//! declaration carries a `Description` and the type's own `Tags`, and which published return
//! value the stack's output reads (`Ref`, `Fn::GetAtt TrainingDatasetArn`). Every row reads the
//! `Fn::GetAtt Status` the owning service reports beside it, so no row can pass with an output
//! that has a key and no value.
//!
//! The refusal rows differ along the CLASS of location Clean Rooms ML will not train from: one
//! nothing was ever staged under, one holding only a zero-sized object, one whose objects
//! live under a SIBLING prefix that merely starts with the same characters, and one naming an
//! object outright that the bucket does not hold — whose refusal names the location as the table
//! states it. Each is the owning service's own refusal, reported on the resource that failed —
//! never the `Template format error: Unrecognized resource types` a type CloudFormation does not
//! know is refused with, which no row of either table may ever see.
//!
//! Every expected value is computed here from what the row declares, from the Region each
//! request is signed for and from the account every ARN this emulator mints carries — never read
//! back out of the answer being checked.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};
// The CloudFormation wire and the stack waits the sibling suite about a completed stack's
// outputs already owns, so neither suite can drift about what settling is.
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, reported_failures, settled_status, Answered, ACCOUNT,
    REGION,
};

/// The statuses the rule is stated about: the update that adds the dataset, the update that is
/// rolled back because the resource failed, and the deletion that takes it away.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
pub(crate) const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The logical ids every template below declares.
const BUCKET: &str = "DataBucket";
pub(crate) const DATABASE: &str = "TrainingCatalog";
pub(crate) const TABLE: &str = "TrainingTable";
pub(crate) const DATASET: &str = "TrainingDataset";

/// The outputs the deploy table reads the dataset's identity through, and the one it reads the
/// status Clean Rooms ML reports for it through.
pub(crate) const REF_OUTPUT: &str = "DatasetRef";
pub(crate) const ARN_OUTPUT: &str = "DatasetArn";
pub(crate) const STATUS_OUTPUT: &str = "DatasetStatus";

/// The two `Fn::GetAtt` attributes the type publishes.
pub(crate) const ARN_ATTRIBUTE: &str = "TrainingDatasetArn";
pub(crate) const STATUS_ATTRIBUTE: &str = "Status";

/// The role every declaration names as the one Clean Rooms ML reads the data with. A literal,
/// so what each row varies is the training data and not the role: the member is modelled
/// `^arn:aws[-a-z]*:iam::[0-9]{12}:role/.+$`.
pub(crate) const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/clean-rooms-ml-training";

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, a value of the
/// CloudFormation model's own `HandlerErrorCode` enum.
pub(crate) const INVALID_REQUEST: &str = "InvalidRequest";

/// The SDK's own name for Clean Rooms ML, as the exception its SDK raises states it.
const CLEANROOMSML_SERVICE: &str = "CleanRoomsMl";

/// The HTTP status Clean Rooms ML refuses a `CreateTrainingDataset` whose request parameters
/// are incorrect with: `ValidationException`'s own `httpError`
/// (`smithy_query(query="operation",operation="CreateTrainingDataset")`).
const REFUSED_STATUS: u16 = 400;

/// The error code and status Clean Rooms ML answers a read of a training dataset it does not
/// hold with: `ResourceNotFoundException`'s own `httpError`
/// (`smithy_query(query="operation",operation="GetTrainingDataset")`).
const DATASET_ABSENT: (u16, &str) = (404, "ResourceNotFoundException");

/// The status Clean Rooms ML reports a training dataset in: `TrainingDatasetStatus` models
/// `ACTIVE` and nothing else.
pub(crate) const ACTIVE: &str = "ACTIVE";

/// The bytes a row stages as the training data, and the key a staged object carries under a
/// prefix. The content is never read by anything under test — what the rule turns on is that
/// the object is NOT zero-sized.
pub(crate) const RECORDS: &[u8] = b"user-1,item-1,2026-01-01T00:00:00Z\n";
const PART_KEY: &str = "part-00000.csv";

/// The services this suite exercises, mounted on ONE engine exactly as `core/router/src/lib.rs`
/// mounts them: the stack's declared resources are really created in the services that own them,
/// and Clean Rooms ML resolves the Glue table and the objects its location holds through those
/// services' own state.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cleanroomsml_awsstarkcontrolservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// Call one Amazon S3 operation over its REST surface, path-style — the addressing the emulated
/// service routes on.
pub(crate) async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &'static [u8],
) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from_static(body),
    )
    .await
}

/// Stage `body` at `bucket`/`key`, asserting Amazon S3 took the write.
pub(crate) async fn stage_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &'static [u8],
) {
    let staged = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        staged.status,
        200,
        "case {case:?}: expected Amazon S3 to hold {key:?} in {bucket:?}, got {} with {:?}",
        staged.status,
        staged.text()
    );
}

/// Call one Clean Rooms ML operation over its restJson1 surface.
async fn cleanroomsml(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("cleanrooms-ml.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("cleanrooms-ml")),
        ],
        Bytes::new(),
    )
    .await
}

/// `GetTrainingDataset` for one ARN, as that service's model binds it:
/// `GET /training-dataset/{trainingDatasetArn}`, the label escaped as a single path segment so
/// the ARN's own `:` and `/` address one dataset rather than another path.
pub(crate) async fn get_training_dataset(router: &MultiServiceRouter, arn: &str) -> Answered {
    cleanroomsml(router, "GET", &format!("/training-dataset/{}", form_encode(arn))).await
}

/// Every training dataset ARN `ListTrainingDatasets` reports, which is what tells a dataset that
/// exists from one nothing created.
pub(crate) async fn listed_training_datasets(
    router: &MultiServiceRouter,
    case: &str,
) -> Vec<String> {
    let answered = cleanroomsml(router, "GET", "/training-dataset").await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from ListTrainingDatasets, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("trainingDatasets")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|summary| summary.get(ARN_ATTRIBUTE_MEMBER).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The member Clean Rooms ML reports a training dataset's ARN in, on a summary as on a read.
pub(crate) const ARN_ATTRIBUTE_MEMBER: &str = "trainingDatasetArn";

/// Where one row's Glue table stores its data, and what is staged in the bucket under it.
#[derive(Clone, Copy)]
struct TrainingData {
    /// The key suffix the table's `StorageDescriptor$Location` names after the bucket, as the
    /// declaration spells it.
    location_suffix: &'static str,
    /// The key the row stages the records under, or `None` where it stages nothing.
    staged_key: Option<&'static str>,
}

/// One row of the deploy table.
struct Deployed {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    database: &'static str,
    table: &'static str,
    /// The name the declaration gives the dataset, which that service holds it under.
    dataset: &'static str,
    data: TrainingData,
    /// Whether the declaration states the catalog the table lives in, or leaves it to the
    /// service (which resolves it to the calling account's own).
    states_catalog: bool,
    /// The description the declaration carries, where it carries one.
    description: Option<&'static str>,
    /// The type's own tags the declaration carries.
    tags: &'static [(&'static str, &'static str)],
    /// The schema the dataset declares: one `(column name, column type)` per column.
    schema: &'static [(&'static str, &'static str)],
    /// Which output the case reads the dataset's ARN out of.
    arn_output: &'static str,
}

/// The column types below are `ColumnType`'s own values
/// (`smithy_query(query="shape",shape="ColumnType")`: `USER_ID`, `ITEM_ID`, `TIMESTAMP`,
/// `CATEGORICAL_FEATURE`, `NUMERICAL_FEATURE`).
const DEPLOYED: &[Deployed] = &[
    Deployed {
        case: "a table whose location is a nested prefix holding the records",
        stack: "cleanroomsml-training-dataset-prefix",
        bucket: "cleanroomsml-training-prefix-bucket",
        database: "cleanroomsml_training_prefix_db",
        table: "training_events",
        dataset: "prefix-training-dataset",
        data: TrainingData {
            location_suffix: "data/",
            staged_key: Some("data/part-00000.csv"),
        },
        states_catalog: true,
        description: Some("the interactions the prefix holds"),
        tags: &[("Tier", "analytics"), ("Owner", "platform")],
        schema: &[
            ("user_id", "USER_ID"),
            ("item_id", "ITEM_ID"),
            ("timestamp", "TIMESTAMP"),
        ],
        arn_output: REF_OUTPUT,
    },
    Deployed {
        case: "a table whose location names the object outright, with no description or tags",
        stack: "cleanroomsml-training-dataset-object",
        bucket: "cleanroomsml-training-object-bucket",
        database: "cleanroomsml_training_object_db",
        table: "interaction_log",
        dataset: "object-training-dataset",
        data: TrainingData {
            location_suffix: "events.csv",
            staged_key: Some("events.csv"),
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
        arn_output: ARN_OUTPUT,
    },
    Deployed {
        case: "a table at the bucket's root, read through Fn::GetAtt",
        stack: "cleanroomsml-training-dataset-root",
        bucket: "cleanroomsml-training-root-bucket",
        database: "cleanroomsml_training_root_db",
        table: "root_events",
        dataset: "root-training-dataset",
        data: TrainingData {
            location_suffix: "",
            staged_key: Some(PART_KEY),
        },
        states_catalog: false,
        description: Some("the interactions the bucket holds"),
        tags: &[("Tier", "research")],
        schema: &[("user_id", "USER_ID"), ("item_id", "ITEM_ID")],
        arn_output: ARN_OUTPUT,
    },
];

/// One row of the refusal table: the same deployment, differing only in what the table's
/// location holds.
struct Refused {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    database: &'static str,
    table: &'static str,
    dataset: &'static str,
    data: TrainingData,
}

const REFUSED: &[Refused] = &[
    Refused {
        // The recorded shape: the bucket is there and the prefix the table names is empty.
        case: "a prefix nothing was ever staged under",
        stack: "cleanroomsml-training-dataset-empty",
        bucket: "cleanroomsml-training-empty-bucket",
        database: "cleanroomsml_training_empty_db",
        table: "empty_events",
        dataset: "empty-training-dataset",
        data: TrainingData {
            location_suffix: "data/",
            staged_key: None,
        },
    },
    Refused {
        case: "a prefix holding only a zero-sized object",
        stack: "cleanroomsml-training-dataset-zero",
        bucket: "cleanroomsml-training-zero-bucket",
        database: "cleanroomsml_training_zero_db",
        table: "zero_events",
        dataset: "zero-training-dataset",
        data: TrainingData {
            location_suffix: "data/",
            staged_key: Some("data/part-00000.csv"),
        },
    },
    Refused {
        case: "a prefix whose records live under a sibling prefix of the same opening",
        stack: "cleanroomsml-training-dataset-sibling",
        bucket: "cleanroomsml-training-sibling-bucket",
        database: "cleanroomsml_training_sibling_db",
        table: "sibling_events",
        dataset: "sibling-training-dataset",
        data: TrainingData {
            location_suffix: "data/",
            staged_key: Some("data-archive/part-00000.csv"),
        },
    },
    // A location that names the object outright rather than a prefix: the refusal names it as
    // the table states it, with no separator appended.
    Refused {
        case: "a location naming an object the bucket does not hold",
        stack: "cleanroomsml-training-dataset-absent-object",
        bucket: "cleanroomsml-training-absent-object-bucket",
        database: "cleanroomsml_training_absent_object_db",
        table: "absent_object_events",
        dataset: "absent-object-training-dataset",
        data: TrainingData {
            location_suffix: "events.csv",
            staged_key: None,
        },
    },
];

/// The location one row's table stores its data at, which is also the location a refusal names:
/// the table's own stored value, character for character.
fn location(bucket: &str, data: &TrainingData) -> String {
    format!("s3://{bucket}/{}", data.location_suffix)
}

/// The sentence Clean Rooms ML refuses a training dataset whose location holds no data with:
/// fixed text, with the location the Glue table states after it. Spelled out here rather than
/// taken from the emulator's own constant, so the suite pins AWS's wording and not the
/// implementation's.
pub(crate) fn no_records_sentence(location: &str) -> String {
    format!("Failed to find any non-zero sized objects in this S3 location {location}.")
}

/// The prerequisites every case deploys first: the bucket the records live in, the Glue database
/// and the table whose location names them. The bucket is RETAINED on deletion, so the stack's
/// own delete is not held up by the objects a case staged in it.
fn prerequisites(bucket: &str, database: &str, table: &str, data: &TrainingData) -> String {
    json!({
        "Description": "the bucket, database and table a training dataset is declared over",
        "Resources": {
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                "DeletionPolicy": "Retain",
                "Properties": { "BucketName": bucket }
            },
            DATABASE: {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": database } }
            },
            TABLE: {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE },
                    "TableInput": {
                        "Name": table,
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Location": location(bucket, data),
                            "Columns": [
                                { "Name": "user_id", "Type": "string" },
                                { "Name": "item_id", "Type": "string" },
                                { "Name": "timestamp", "Type": "string" }
                            ]
                        }
                    }
                }
            }
        }
    })
    .to_string()
}

/// The declared `TrainingData` of one dataset: the one `Dataset` the modelled list admits
/// (`DatasetList` is `@length(min: 1, max: 1)`), over the Glue table the prerequisites created.
pub(crate) fn training_data(table: &str, states_catalog: bool, schema: &[(&str, &str)]) -> Value {
    let mut glue = json!({ "DatabaseName": { "Ref": DATABASE }, "TableName": table });
    if states_catalog {
        glue["CatalogId"] = json!(ACCOUNT);
    }
    json!([{
        // `DatasetType` models `INTERACTIONS` and nothing else.
        "Type": "INTERACTIONS",
        "InputConfig": {
            "Schema": schema
                .iter()
                .map(|(name, kind)| json!({ "ColumnName": name, "ColumnTypes": [kind] }))
                .collect::<Vec<Value>>(),
            "DataSource": { "GlueDataSource": glue }
        }
    }])
}

/// The prerequisites PLUS the training dataset, which is the body the update submits.
#[allow(clippy::too_many_arguments)]
fn with_dataset(
    bucket: &str,
    database: &str,
    table: &str,
    data: &TrainingData,
    dataset: &str,
    states_catalog: bool,
    description: Option<&str>,
    tags: &[(&str, &str)],
    schema: &[(&str, &str)],
) -> String {
    let mut body: Value = serde_json::from_str(&prerequisites(bucket, database, table, data))
        .expect("the prerequisite template is JSON this suite wrote");
    let mut properties = json!({
        "Name": dataset,
        "RoleArn": ROLE_ARN,
        "TrainingData": training_data(table, states_catalog, schema),
    });
    if let Some(description) = description {
        properties["Description"] = json!(description);
    }
    if !tags.is_empty() {
        properties["Tags"] = tags
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect();
    }
    body["Resources"][DATASET] = json!({
        "Type": "AWS::CleanRoomsML::TrainingDataset",
        "DependsOn": TABLE,
        "Properties": properties,
    });
    body["Outputs"] = json!({
        REF_OUTPUT: { "Value": { "Ref": DATASET } },
        ARN_OUTPUT: { "Value": { "Fn::GetAtt": [DATASET, ARN_ATTRIBUTE] } },
        STATUS_OUTPUT: { "Value": { "Fn::GetAtt": [DATASET, STATUS_ATTRIBUTE] } },
    });
    body.to_string()
}

/// Deploy the prerequisites as `stack` and wait for them, asserting the submission and the
/// settled status.
async fn deployed_prerequisites(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) {
    let created = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(router, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the prerequisites to complete, got {settled:?}; the failures \
         it reported: {:?}; last answer {described}",
        reported_failures(router, stack).await
    );
}

/// `UpdateStack` with `template`, asserting the submission itself was ACCEPTED — the type the
/// body adds is one a template may declare, so the answer carries a `StackId` rather than a
/// template-format error — and answering the settled status with the `DescribeStacks` body it
/// was read from.
async fn updated_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> (String, String) {
    let updated = cloudformation(
        router,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    let body = updated.text();
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack \u{2014} an \
         AWS::CleanRoomsML::TrainingDataset is a type a template may declare \u{2014} got {} with \
         {body}",
        updated.status
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    assert!(
        opt_element(&body, "StackId").is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the update to answer the stack it revised, got {body}"
    );
    settled_status(router, stack).await
}

/// Every `ResourceStatusReason` the stack recorded for a FAILED event of `logical_id`, in
/// document order.
pub(crate) async fn failure_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<String> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {} with {body:?}",
        answered.status
    );
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let failed = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status.contains("FAILED"));
            (named && failed)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
        })
        .collect()
}

/// Split one failure reason into the exception the resource's handler quoted and the
/// `HandlerErrorCode` class it reported the refusal under.
pub(crate) fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape a Clean Rooms ML SDK raises — the service's own
/// sentence, then the SDK's diagnostic of the refused call (its name for the service, the HTTP
/// status, the id of the call and the attempts it made) — and answer the sentence.
pub(crate) fn asserted_sentence(case: &str, quoted: &str) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this service's SDK words after the \
             envelope, got {quoted:?}"
        )
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], CLEANROOMSML_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_STATUS}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    assert!(
        fields[2].starts_with("Request ID: ") && fields[2].len() > "Request ID: ".len(),
        "case {case:?}: expected the id of the call, got {:?}",
        fields[2]
    );
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// The ARN prefix a training dataset of this account and Region carries
/// (`TrainingDatasetArn`:
/// `^arn:aws[-a-z]*:cleanrooms-ml:[-a-z0-9]+:[0-9]{12}:training-dataset/[-a-zA-Z0-9_/.]+$`).
fn training_dataset_arn_prefix() -> String {
    format!("arn:aws:cleanrooms-ml:{REGION}:{ACCOUNT}:training-dataset/")
}

/// Hold `arn` to the form that pattern admits, and answer it.
pub(crate) fn asserted_arn(case: &str, arn: &str) -> String {
    let prefix = training_dataset_arn_prefix();
    let resource = arn.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected an ARN opening with {prefix:?}, got {arn:?}")
    });
    assert!(
        !resource.is_empty()
            && resource
                .chars()
                .all(|held| held.is_ascii_alphanumeric() || "-_/.".contains(held)),
        "case {case:?}: expected the resource segment the model's pattern admits, got {arn:?}"
    );
    arn.to_string()
}

/// An error code as a client reads it off a restJson1 refusal: the shape name, with any Smithy
/// namespace prefix the envelope carries dropped.
fn error_code(answered: &Answered) -> String {
    answered
        .json()
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[tokio::test]
async fn an_update_that_adds_a_training_dataset_records_it_in_clean_rooms_ml() {
    for row in DEPLOYED {
        // One emulator per row: each row deploys its own bucket, catalog and dataset, and a
        // fresh engine is what keeps one row's listing from reporting another's dataset.
        let router = build_router();
        let case = row.case;

        deployed_prerequisites(
            &router,
            case,
            row.stack,
            &prerequisites(row.bucket, row.database, row.table, &row.data),
        )
        .await;
        if let Some(key) = row.data.staged_key {
            stage_object(&router, case, row.bucket, key, RECORDS).await;
        }

        // 1. The update is ACCEPTED and completes: the type is one the registry knows, and the
        // resource is created in the service that owns it.
        let template = with_dataset(
            row.bucket,
            row.database,
            row.table,
            &row.data,
            row.dataset,
            row.states_catalog,
            row.description,
            row.tags,
            row.schema,
        );
        let (settled, described) = updated_stack(&router, case, row.stack, &template).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete, got {settled:?}; the failures it \
             reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
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
        // Every output the template declares carries a VALUE: a published return value with a
        // key and nothing under it is a map a client cannot build. The status is the one Clean
        // Rooms ML reports for the dataset, not one CloudFormation decided.
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
            ("name", row.dataset.to_string()),
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
            row.description,
            "case {case:?}: expected the declared description, got {reported}"
        );
        let dataset = reported
            .pointer("/trainingData/0")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            dataset.get("type").and_then(Value::as_str),
            Some("INTERACTIONS"),
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
            (Some(row.database), Some(row.table), Some(ACCOUNT)),
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
        let mut declared: Vec<(String, String)> = row
            .schema
            .iter()
            .map(|(name, kind)| ((*name).to_string(), (*kind).to_string()))
            .collect();
        declared.sort();
        assert_eq!(
            columns, declared,
            "case {case:?}: expected the declared schema, got {dataset}"
        );
        assert_eq!(
            listed_training_datasets(&router, case).await,
            vec![arn.clone()],
            "case {case:?}: expected the dataset the stack created to be the one this account \
             lists"
        );

        // 4. And deleting the stack takes the dataset away: the resource is removed from the
        // service that holds it, not just from CloudFormation's own record.
        let deleted = cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {} with {:?}",
            deleted.status,
            deleted.text()
        );
        let (settled, described) = settled_status(&router, row.stack).await;
        assert!(
            settled.is_empty() || settled == DELETE_COMPLETE,
            "case {case:?}: expected the deletion to converge to {DELETE_COMPLETE}, got \
             {settled:?} in {described}"
        );
        let read = get_training_dataset(&router, &arn).await;
        assert_eq!(
            (read.status, error_code(&read)),
            (DATASET_ABSENT.0, DATASET_ABSENT.1.to_string()),
            "case {case:?}: expected Clean Rooms ML to hold no dataset once the stack was \
             deleted, got {} with {:?}",
            read.status,
            read.text()
        );
        assert!(
            listed_training_datasets(&router, case).await.is_empty(),
            "case {case:?}: expected this account to list no training dataset once the stack was \
             deleted"
        );
    }
}

#[tokio::test]
async fn a_declaration_clean_rooms_ml_refuses_rolls_the_update_back() {
    for row in REFUSED {
        let router = build_router();
        let case = row.case;

        deployed_prerequisites(
            &router,
            case,
            row.stack,
            &prerequisites(row.bucket, row.database, row.table, &row.data),
        )
        .await;
        if let Some(key) = row.data.staged_key {
            // The zero-sized row stages the key the table's prefix names and NO bytes under it;
            // the sibling row stages bytes under a key the prefix does not name.
            let body: &'static [u8] = match key.starts_with(row.data.location_suffix) {
                true => b"",
                false => RECORDS,
            };
            stage_object(&router, case, row.bucket, key, body).await;
        }

        let template = with_dataset(
            row.bucket,
            row.database,
            row.table,
            &row.data,
            row.dataset,
            true,
            None,
            &[],
            &[("user_id", "USER_ID"), ("item_id", "ITEM_ID")],
        );

        // 1. The update is ACCEPTED — the type is recognised — and then rolled back, because the
        // resource the owning service refused is a failed resource rather than a bad template.
        let (settled, described) = updated_stack(&router, case, row.stack, &template).await;
        assert_eq!(
            settled, UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the update to roll back for a location Clean Rooms ML will \
             not train from, got {settled:?}; the failures it reported: {:?}; last answer \
             {described}",
            reported_failures(&router, row.stack).await
        );
        assert!(
            outputs_of(&described).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // 2. ONE failed event for the dataset, whose reason quotes Clean Rooms ML's own
        // exception, names the location its Glue table states, and is classified as a refusal of
        // the request.
        let reasons = failure_reasons(&router, case, row.stack, DATASET).await;
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: expected exactly one failed event for {DATASET}, got {reasons:?}"
        );
        let (quoted, class) = quoted_and_class(case, &reasons[0]);
        assert_eq!(
            asserted_sentence(case, &quoted),
            no_records_sentence(&location(row.bucket, &row.data)),
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
