// SPDX-License-Identifier: CC0-1.0

//! `CreateDatasetImportJob` answers a MODELLED outcome for every submission, and the
//! provisioning chain it carries is the one the inference plane serves.
//!
//! RULE: a submission of an operation the model declares is answered with one of THAT
//! operation's modelled outcomes — its success output, or one of the error shapes its
//! model lists for it — and never with the emulator's own scaffold refusal (HTTP 501,
//! error code `NotImplemented`), which no AWS operation models; and a create that answers
//! its success output is backed by state, so the resource it minted is readable through
//! its own `Describe` and a refused submission leaves the store as it was.
//!
//! `CreateDatasetImportJob` is step four of the Amazon Personalize provisioning chain
//! (CreateSchema, CreateDatasetGroup, CreateDataset, CreateDatasetImportJob,
//! CreateSolution, CreateSolutionVersion, CreateCampaign), and its model lists
//! `InvalidInputException`, `LimitExceededException`, `ResourceAlreadyExistsException`,
//! `ResourceInUseException`, `ResourceNotFoundException` and `TooManyTagsException` —
//! so a client that cannot get past it can reach no campaign, and the inference plane
//! can only refuse.
//!
//! Three crates have to agree for the rule to hold here, which is why the suite lives
//! with the cross-service tests: the Amazon Personalize control plane records the job,
//! Amazon S3 holds the CSV training data the job reads, and Amazon Personalize Runtime
//! serves the campaign the chain ends in. All three are mounted on ONE shared engine, as
//! the assembled router mounts them.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: the
//! form of the data location (an object, a folder of objects, a non-storage URL, none at
//! all), the import mode (defaulted, `FULL`, `INCREMENTAL`), the job name (fresh, taken,
//! at each published length bound and one past it, outside the published character set),
//! the dataset the job names (one that exists, one that does not) and whether the import
//! can be carried out at all. The recorded case the issue cited — a plain submission
//! against a dataset whose CSV is in object storage — is one row among them.
//!
//! Nothing here asserts a value this emulator invented: an ARN is checked against the
//! documented Amazon Personalize ARN layout for the name the request chose, a status
//! against the states the model documents, and every error code is read out of the
//! answer.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for and addressed in.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = emulator_server::EMULATOR_ACCOUNT_ID;

/// The bucket the CSV training data is uploaded to.
const BUCKET: &str = "personalize-import-outcomes";

/// The status line and error code the emulator's scaffold channel answers an
/// unimplemented operation with. No AWS operation models either, so no row may see them.
const SCAFFOLD_STATUS: u16 = 501;
const SCAFFOLD_CODE: &str = "NotImplemented";

/// The one status an `awsJson1_1` service puts on a client error; the Amazon Personalize
/// API Reference lists "HTTP Status Code: 400" for each of this service's error shapes.
const CLIENT_ERROR_STATUS: u16 = 400;

/// The states a dataset import job passes through, and the two it ends in. Source: the
/// `CreateDatasetImportJob` documentation — "CREATE PENDING > CREATE IN_PROGRESS > ACTIVE
/// -or- CREATE FAILED".
const TRANSIENT_STATUSES: &[&str] = &["CREATE PENDING", "CREATE IN_PROGRESS"];
const STATUS_ACTIVE: &str = "ACTIVE";
const STATUS_CREATE_FAILED: &str = "CREATE FAILED";

/// The import modes `importMode` admits, and the one that applies when the request names
/// none: "The default import mode is FULL."
const FULL: &str = "FULL";
const INCREMENTAL: &str = "INCREMENTAL";

/// The published bounds of `jobName`, whose target carries `@length(1, 63)` and
/// `@pattern("^[a-zA-Z0-9][a-zA-Z0-9\-_]*$")`. Source:
/// `smithy_query(query="operation", operation="CreateDatasetImportJob")`.
const JOB_NAME_MIN: usize = 1;
const JOB_NAME_MAX: usize = 63;

/// A `PERSONALIZED_RANKING` recipe, which is the recipe type `GetPersonalizedRanking`
/// serves. Source: the catalog the control plane resolves `recipeArn` against
/// (`service_com_amazonaws_personalize_amazonpersonalize::state::pipeline::RECIPES`).
const RANKING_RECIPE: &str = "arn:aws:personalize:::recipe/aws-personalized-ranking";

/// How long a case waits for a stage to reach a terminal status, and how often it looks
/// while waiting. The budget is far above any span this emulator owes a create, so a
/// loaded machine has room; the wait is for the CONDITION, never for a span.
const PATIENCE: Duration = Duration::from_secs(60);
const LOOK: Duration = Duration::from_millis(20);

/// The Amazon Personalize control plane, Amazon Personalize Runtime and Amazon S3 over
/// ONE shared engine, as `core/router/src/lib.rs` mounts them — so a job's import reads
/// the very objects this suite uploads, and a campaign the chain deploys is one the
/// inference plane can resolve.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are verified by the emulator, never by a service; this suite is about
    // behaviour, so it signs with a fixed scope and verifies nothing. The header is still
    // what routes the request and names the Region, exactly as a signed client's would.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_personalize_amazonpersonalize::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header scoped to `signing_name` in [`REGION`], which is what
/// routes the request and what a service reads the caller's Region from.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// A response body as JSON, or `Null` for an empty one.
fn parse(body: &Bytes) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    }
}

/// One `awsJson1_1` control-plane call (`POST /` with `X-Amz-Target`).
async fn personalize(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("personalize.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonPersonalize.{operation}"))
        .header("authorization", authorization("personalize"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), parse(response.body()))
}

/// One `restJson1` inference call (`POST <route>` on the runtime endpoint).
async fn runtime(router: &MultiServiceRouter, route: &str, body: &Value) -> (u16, Value) {
    let host = format!("personalize-runtime.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{route}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("personalize"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), parse(response.body()))
}

/// One path-style request to the emulator's Amazon S3 front door: the bucket and the key
/// live in the URI and the body is the object's bytes.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> u16 {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    router.dispatch(request).await.status().as_u16()
}

/// Create [`BUCKET`] if it is not there yet and upload one object into it.
///
/// The create names no `LocationConstraint`: the unspecified constraint stands for
/// us-east-1, which is the Region this suite signs for.
async fn upload(router: &MultiServiceRouter, key: &str, body: &str) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        created == 200 || created == 409,
        "case \"upload {key}\": expected CreateBucket to answer 200 or 409, got {created}"
    );
    let put = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body.as_bytes()).await;
    assert_eq!(
        put, 200,
        "case \"upload {key}\": expected PutObject to answer 200, got {put}"
    );
}

/// The `__type` discriminator of a failure body, with any namespace stripped, or
/// `"<no __type>"`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a failure body, or `"<no message>"`.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Assert `operation` with `body` answered neither the scaffold status nor the scaffold
/// error code, and return the answer.
///
/// This is the rule the suite is about, so it is asserted for EVERY call it makes, not
/// only for the operation under test.
async fn modelled(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = personalize(router, operation, body).await;
    assert_ne!(
        status, SCAFFOLD_STATUS,
        "case {case:?}: expected {operation} to answer a modelled outcome, got HTTP \
         {SCAFFOLD_STATUS} {answered}"
    );
    let code = error_code(&answered);
    assert_ne!(
        code, SCAFFOLD_CODE,
        "case {case:?}: expected {operation} to answer a modelled outcome, got {code} \
         {:?}",
        error_message(&answered)
    );
    (status, answered)
}

/// A control-plane call that must succeed, returning its response.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = modelled(router, case, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} {answered}"
    );
    answered
}

/// A control-plane call that must succeed, returning the ARN it reports in `arn_member`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    arn_member: &str,
) -> String {
    let answered = ok(router, case, operation, body).await;
    answered[arn_member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected {operation} to report {arn_member}, got {answered}"))
        .to_string()
}

/// Poll `operation` until the resource it reports leaves the transient create statuses,
/// and return that resource.
///
/// The loop waits for the CONDITION: every create of this service finishes its own work
/// before it answers, so the first read is normally already terminal, and the deadline is
/// what turns a stage that is genuinely stuck into a report instead of a suite that never
/// returns.
async fn await_terminal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    arn_member: &str,
    resource_member: &str,
    arn: &str,
) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let answered = ok(router, case, operation, &json!({arn_member: arn})).await;
        let resource = answered[resource_member].clone();
        let reported = resource["status"].as_str().unwrap_or("").to_string();
        if !reported.is_empty() && !TRANSIENT_STATUSES.contains(&reported.as_str()) {
            return resource;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {operation} to report a terminal status, it never left \
             {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Await a terminal status and assert it is the one `expected`.
async fn await_status(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    arn_member: &str,
    resource_member: &str,
    arn: &str,
    expected: &str,
) -> Value {
    let resource = await_terminal(router, case, operation, arn_member, resource_member, arn).await;
    assert_eq!(
        resource["status"].as_str().unwrap_or(""),
        expected,
        "case {case:?}: expected {operation} to report {expected}, got {resource}"
    );
    resource
}

/// The ARN Amazon Personalize gives a dataset import job named `job_name`:
/// `arn:aws:personalize:<region>:<account>:dataset-import-job/<jobName>`.
fn import_job_arn(job_name: &str) -> String {
    format!("arn:aws:personalize:{REGION}:{ACCOUNT}:dataset-import-job/{job_name}")
}

/// An Avro record schema for `dataset_type` declaring `columns`, in the Avro JSON format
/// `CreateSchema` documents.
fn avro_schema(dataset_type: &str, columns: &[&str]) -> String {
    let fields: Vec<Value> = columns
        .iter()
        .map(|column| json!({"name": column, "type": "string"}))
        .collect();
    json!({
        "type": "record",
        "name": dataset_type,
        "namespace": "com.amazonaws.personalize.schema",
        "fields": fields,
        "version": "1.0",
    })
    .to_string()
}

/// The columns an Interactions schema and its CSV files carry. The first three are the
/// fields Amazon Personalize requires of an interactions schema (user, item, time).
const INTERACTIONS_COLUMNS: &[&str] = &["USER_ID", "ITEM_ID", "TIMESTAMP", "EVENT_TYPE"];

/// The id of the `index`-th fixture item.
fn item_id(index: usize) -> String {
    format!("item-{index:02}")
}

/// A CSV of interactions over `items`, in the comma-delimited form with a header row
/// naming the schema's fields that Amazon Personalize imports.
fn interactions_csv(items: &[String]) -> String {
    let mut csv = format!("{}\n", INTERACTIONS_COLUMNS.join(","));
    for (index, item) in items.iter().enumerate() {
        for occurrence in 0..=index {
            csv.push_str(&format!(
                "user-{},{item},{},watch\n",
                occurrence % 3 + 1,
                1_700_000_000 + occurrence
            ));
        }
    }
    csv
}

/// Create a dataset group, wait for it to become `ACTIVE`, and return its ARN.
async fn active_group(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let arn = created(
        router,
        case,
        "CreateDatasetGroup",
        &json!({"name": name}),
        "datasetGroupArn",
    )
    .await;
    await_status(
        router,
        case,
        "DescribeDatasetGroup",
        "datasetGroupArn",
        "datasetGroup",
        &arn,
        STATUS_ACTIVE,
    )
    .await;
    arn
}

/// Create an Interactions schema and a dataset of that type in `group`, and return the
/// dataset's ARN.
async fn interactions_dataset(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> String {
    let schema = created(
        router,
        case,
        "CreateSchema",
        &json!({
            "name": format!("schema-{name}"),
            "schema": avro_schema("Interactions", INTERACTIONS_COLUMNS),
        }),
        "schemaArn",
    )
    .await;
    created(
        router,
        case,
        "CreateDataset",
        &json!({
            "name": format!("dataset-{name}"),
            "datasetGroupArn": group,
            "datasetType": "Interactions",
            "schemaArn": schema,
        }),
        "datasetArn",
    )
    .await
}

/// What a submission of `CreateDatasetImportJob` must be answered with.
#[derive(Debug, Clone)]
enum Outcome {
    /// Accepted, and the import it ran carried the data: the job's terminal status is
    /// `ACTIVE` and it reports the import mode named here.
    Imported { import_mode: &'static str },
    /// Accepted, and the import it ran could not be carried out: the job's terminal
    /// status is `CREATE FAILED`, carrying the reason.
    Failed,
    /// Refused with this modelled error shape. `resolved_before` says whether the ARN the
    /// job name mints already names a job — true only where the collision is the reason
    /// for the refusal — so the row can assert the refusal stored nothing.
    Refused {
        code: &'static str,
        resolved_before: bool,
    },
}

/// One row of the submission table.
struct Row {
    case: String,
    job_name: String,
    body: Value,
    outcome: Outcome,
}

#[tokio::test]
async fn every_dataset_import_job_submission_is_answered_with_a_modelled_outcome() {
    let router = emulator();
    let setup = "import-outcome setup";

    // The training data: one object, and a folder holding two objects, so the data
    // location's form is something the table can vary.
    let items: Vec<String> = (1..=6).map(item_id).collect();
    upload(&router, "single/interactions.csv", &interactions_csv(&items)).await;
    upload(&router, "folder/part-1.csv", &interactions_csv(&items[..3])).await;
    upload(&router, "folder/part-2.csv", &interactions_csv(&items[3..])).await;
    let object = format!("s3://{BUCKET}/single/interactions.csv");
    let folder = format!("s3://{BUCKET}/folder/");
    let empty = format!("s3://{BUCKET}/nothing-here/");

    // Two datasets: one the accepted submissions import into, and one nothing has ever
    // been imported into, which is what the first-import rules are about. A dataset group
    // holds at most one dataset of a type, so each of them lives in its own group.
    let group = active_group(&router, setup, "import-outcomes").await;
    let spare_group = active_group(&router, setup, "import-outcomes-spare").await;
    let imported_into = interactions_dataset(&router, setup, &group, "imported").await;
    let untouched = interactions_dataset(&router, setup, &spare_group, "untouched").await;
    let absent_dataset =
        format!("arn:aws:personalize:{REGION}:{ACCOUNT}:dataset/absent-group/INTERACTIONS");
    let role = format!("arn:aws:iam::{ACCOUNT}:role/PersonalizeImport");

    // The rows run in order: the name-collision row needs the name it collides with to
    // have been taken, and the INCREMENTAL row needs a bulk import to have completed.
    let taken_name = "import-an-object".to_string();
    let rows: Vec<Row> = vec![
        Row {
            case: "an object key, with no importMode".to_string(),
            job_name: taken_name.clone(),
            body: json!({
                "jobName": taken_name,
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
                "roleArn": role,
            }),
            // "The default import mode is FULL."
            outcome: Outcome::Imported { import_mode: FULL },
        },
        Row {
            case: "a folder of two objects, importMode FULL".to_string(),
            job_name: "import-a-folder".to_string(),
            body: json!({
                "jobName": "import-a-folder",
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": folder},
                "roleArn": role,
                "importMode": FULL,
            }),
            outcome: Outcome::Imported { import_mode: FULL },
        },
        Row {
            case: "importMode INCREMENTAL once a bulk import has completed".to_string(),
            job_name: "import-incremental".to_string(),
            body: json!({
                "jobName": "import-incremental",
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
                "importMode": INCREMENTAL,
            }),
            outcome: Outcome::Imported {
                import_mode: INCREMENTAL,
            },
        },
        Row {
            // The shortest name the published `@length` admits.
            case: format!("a jobName of the shortest modelled length ({JOB_NAME_MIN})"),
            job_name: "j".repeat(JOB_NAME_MIN),
            body: json!({
                "jobName": "j".repeat(JOB_NAME_MIN),
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Imported { import_mode: FULL },
        },
        Row {
            // The longest name it admits.
            case: format!("a jobName of the longest modelled length ({JOB_NAME_MAX})"),
            job_name: "j".repeat(JOB_NAME_MAX),
            body: json!({
                "jobName": "j".repeat(JOB_NAME_MAX),
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Imported { import_mode: FULL },
        },
        Row {
            // One character past it.
            case: format!("a jobName one character past the modelled length ({})", JOB_NAME_MAX + 1),
            job_name: "j".repeat(JOB_NAME_MAX + 1),
            body: json!({
                "jobName": "j".repeat(JOB_NAME_MAX + 1),
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Refused {
                code: "ValidationException",
                resolved_before: false,
            },
        },
        Row {
            // Outside the published character set: the name may not open with `-`.
            case: "a jobName outside the modelled character set".to_string(),
            job_name: "-leading-hyphen".to_string(),
            body: json!({
                "jobName": "-leading-hyphen",
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Refused {
                code: "ValidationException",
                resolved_before: false,
            },
        },
        Row {
            case: "a jobName another job already took".to_string(),
            job_name: taken_name.clone(),
            body: json!({
                "jobName": taken_name,
                "datasetArn": imported_into,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Refused {
                code: "ResourceAlreadyExistsException",
                resolved_before: true,
            },
        },
        Row {
            case: "a datasetArn no dataset exists for".to_string(),
            job_name: "import-into-nothing".to_string(),
            body: json!({
                "jobName": "import-into-nothing",
                "datasetArn": absent_dataset,
                "dataSource": {"dataLocation": object},
            }),
            outcome: Outcome::Refused {
                code: "ResourceNotFoundException",
                resolved_before: false,
            },
        },
        Row {
            // "If you haven't imported bulk records into the dataset previously, you can
            // only specify FULL."
            case: "importMode INCREMENTAL into a dataset nothing was imported into"
                .to_string(),
            job_name: "import-first-incremental".to_string(),
            body: json!({
                "jobName": "import-first-incremental",
                "datasetArn": untouched,
                "dataSource": {"dataLocation": object},
                "importMode": INCREMENTAL,
            }),
            outcome: Outcome::Refused {
                code: "InvalidInputException",
                resolved_before: false,
            },
        },
        Row {
            case: "a dataLocation that names no Amazon S3 storage".to_string(),
            job_name: "import-over-https".to_string(),
            body: json!({
                "jobName": "import-over-https",
                "datasetArn": untouched,
                "dataSource": {"dataLocation": "https://example.invalid/interactions.csv"},
            }),
            outcome: Outcome::Refused {
                code: "InvalidInputException",
                resolved_before: false,
            },
        },
        Row {
            case: "a dataSource naming no dataLocation at all".to_string(),
            job_name: "import-from-nowhere".to_string(),
            body: json!({
                "jobName": "import-from-nowhere",
                "datasetArn": untouched,
                "dataSource": {},
            }),
            outcome: Outcome::Refused {
                code: "InvalidInputException",
                resolved_before: false,
            },
        },
        Row {
            case: "an Amazon S3 path holding no object".to_string(),
            job_name: "import-nothing-found".to_string(),
            body: json!({
                "jobName": "import-nothing-found",
                "datasetArn": untouched,
                "dataSource": {"dataLocation": empty},
            }),
            outcome: Outcome::Failed,
        },
    ];

    for row in rows {
        let Row {
            case,
            job_name,
            body,
            outcome,
        } = row;
        let arn = import_job_arn(&job_name);
        // Read before the submission, so a refusal can be shown to have changed nothing.
        let before = personalize(
            &router,
            "DescribeDatasetImportJob",
            &json!({"datasetImportJobArn": arn}),
        )
        .await;

        let (status, answered) =
            modelled(&router, &case, "CreateDatasetImportJob", &body).await;

        match outcome {
            Outcome::Imported { import_mode } => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected CreateDatasetImportJob to answer 200, got \
                     {status} {answered}"
                );
                assert_eq!(
                    answered["datasetImportJobArn"].as_str().unwrap_or(""),
                    arn,
                    "case {case:?}: expected the ARN of the name the request chose, got \
                     {answered}"
                );
                let job = await_status(
                    &router,
                    &case,
                    "DescribeDatasetImportJob",
                    "datasetImportJobArn",
                    "datasetImportJob",
                    &arn,
                    STATUS_ACTIVE,
                )
                .await;
                // The create is backed by state: every member the submission settled is
                // read back off the stored job.
                assert_eq!(
                    job["jobName"].as_str().unwrap_or(""),
                    job_name,
                    "case {case:?}: expected the submitted jobName, got {job}"
                );
                assert_eq!(
                    job["datasetArn"].as_str().unwrap_or(""),
                    body["datasetArn"].as_str().unwrap_or(""),
                    "case {case:?}: expected the submitted datasetArn, got {job}"
                );
                assert_eq!(
                    job["dataSource"]["dataLocation"].as_str().unwrap_or(""),
                    body["dataSource"]["dataLocation"].as_str().unwrap_or(""),
                    "case {case:?}: expected the submitted dataLocation, got {job}"
                );
                assert_eq!(
                    job["importMode"].as_str().unwrap_or(""),
                    import_mode,
                    "case {case:?}: expected importMode {import_mode}, got {job}"
                );
                assert!(
                    job.get("failureReason").is_none_or(Value::is_null),
                    "case {case:?}: expected no failureReason on a completed import, got {job}"
                );
            }
            Outcome::Failed => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected CreateDatasetImportJob to answer 200, got \
                     {status} {answered}"
                );
                let job = await_status(
                    &router,
                    &case,
                    "DescribeDatasetImportJob",
                    "datasetImportJobArn",
                    "datasetImportJob",
                    &arn,
                    STATUS_CREATE_FAILED,
                )
                .await;
                let reason = job["failureReason"].as_str().unwrap_or("");
                assert!(
                    !reason.is_empty(),
                    "case {case:?}: expected a failureReason naming what stopped the import, \
                     got {job}"
                );
            }
            Outcome::Refused {
                code,
                resolved_before,
            } => {
                assert_eq!(
                    status, CLIENT_ERROR_STATUS,
                    "case {case:?}: expected HTTP {CLIENT_ERROR_STATUS}, got {status} \
                     {answered}"
                );
                let answered_code = error_code(&answered);
                assert_eq!(
                    answered_code, code,
                    "case {case:?}: expected {code}, got {answered_code} {:?}",
                    error_message(&answered)
                );
                assert!(
                    !error_message(&answered).is_empty(),
                    "case {case:?}: expected the refusal to carry a message, got {answered}"
                );
                // The refusal left the store as it was: the ARN the name mints resolves
                // exactly as it did before the submission.
                let (after_status, after) = personalize(
                    &router,
                    "DescribeDatasetImportJob",
                    &json!({"datasetImportJobArn": arn}),
                )
                .await;
                assert_eq!(
                    after_status, before.0,
                    "case {case:?}: expected the refusal to change nothing, DescribeDatasetImportJob \
                     answered {} before and {after_status} after: {after}",
                    before.0
                );
                if resolved_before {
                    assert_eq!(
                        after_status, 200,
                        "case {case:?}: expected the job the name already named to be readable, \
                         got {after_status} {after}"
                    );
                } else {
                    assert_eq!(
                        after_status, CLIENT_ERROR_STATUS,
                        "case {case:?}: expected the refused name to hold no job, got \
                         {after_status} {after}"
                    );
                    assert_eq!(
                        error_code(&after),
                        "ResourceNotFoundException",
                        "case {case:?}: expected the refused name to hold no job, got {after}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn the_chain_the_import_carries_ends_in_a_campaign_the_inference_plane_serves() {
    let router = emulator();
    let case = "a personalized-ranking round trip over the provisioning chain";

    // Steps 1 to 4 of the chain: a dataset group, an Interactions dataset, and the bulk
    // import that puts the training records on it.
    let items: Vec<String> = (1..=8).map(item_id).collect();
    upload(&router, "ranking/interactions.csv", &interactions_csv(&items)).await;
    let group = active_group(&router, case, "ranking-chain").await;
    let dataset = interactions_dataset(&router, case, &group, "ranking").await;
    let job = created(
        &router,
        case,
        "CreateDatasetImportJob",
        &json!({
            "jobName": "ranking-import",
            "datasetArn": dataset,
            "dataSource": {"dataLocation": format!("s3://{BUCKET}/ranking/interactions.csv")},
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/PersonalizeImport"),
        }),
        "datasetImportJobArn",
    )
    .await;
    await_status(
        &router,
        case,
        "DescribeDatasetImportJob",
        "datasetImportJobArn",
        "datasetImportJob",
        &job,
        STATUS_ACTIVE,
    )
    .await;

    // Steps 5 to 7: a solution trained with a PERSONALIZED_RANKING recipe, a version of
    // it, and the campaign that deploys that version.
    let solution = created(
        &router,
        case,
        "CreateSolution",
        &json!({
            "name": "ranking-solution",
            "datasetGroupArn": group,
            "recipeArn": RANKING_RECIPE,
        }),
        "solutionArn",
    )
    .await;
    let version = created(
        &router,
        case,
        "CreateSolutionVersion",
        &json!({"solutionArn": solution}),
        "solutionVersionArn",
    )
    .await;
    await_status(
        &router,
        case,
        "DescribeSolutionVersion",
        "solutionVersionArn",
        "solutionVersion",
        &version,
        STATUS_ACTIVE,
    )
    .await;
    let campaign = created(
        &router,
        case,
        "CreateCampaign",
        &json!({"name": "ranking-campaign", "solutionVersionArn": version}),
        "campaignArn",
    )
    .await;
    await_status(
        &router,
        case,
        "DescribeCampaign",
        "campaignArn",
        "campaign",
        &campaign,
        STATUS_ACTIVE,
    )
    .await;

    // The inference the chain made reachable: "the ranking of the items that you
    // specified", so the reported list is the same SET of ids, scored descending, under a
    // recommendation id.
    let input_list: Vec<String> = items.iter().rev().cloned().collect();
    let (status, answered) = runtime(
        &router,
        "/personalize-ranking",
        &json!({"campaignArn": campaign, "userId": "user-1", "inputList": input_list}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetPersonalizedRanking to answer 200, got {status} \
         {answered}"
    );
    let ranked = answered["personalizedRanking"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a personalizedRanking, got {answered}"));
    let reported: BTreeSet<String> = ranked
        .iter()
        .map(|entry| {
            entry["itemId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an itemId, got {entry}"))
                .to_string()
        })
        .collect();
    let submitted: BTreeSet<String> = input_list.iter().cloned().collect();
    assert_eq!(
        reported, submitted,
        "case {case:?}: expected the ranking to be a permutation of the inputList, got \
         {answered}"
    );
    let scores: Vec<f64> = ranked
        .iter()
        .map(|entry| {
            entry["score"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a score, got {entry}"))
        })
        .collect();
    for pair in scores.windows(2) {
        assert!(
            pair[0] >= pair[1],
            "case {case:?}: expected the scores not to increase, got {scores:?}"
        );
    }
    assert!(
        !answered["recommendationId"].as_str().unwrap_or("").is_empty(),
        "case {case:?}: expected a non-empty recommendationId, got {answered}"
    );
}
