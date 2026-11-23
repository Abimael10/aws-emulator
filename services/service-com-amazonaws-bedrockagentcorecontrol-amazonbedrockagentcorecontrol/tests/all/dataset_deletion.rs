// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteDataset`.
//!
//! The rule under test: a delete ACCEPTS the removal of exactly what the request named
//! and the clock owes it one span later — the dataset record with its whole version
//! history when the request carries no `datasetVersion`, only the snapshot it names when
//! it carries a published version, and the working copy's examples when it names `DRAFT`
//! — reporting `DELETING` in the meantime, in which reads are still served and writes
//! conflict, and answering the modelled 404 once the removal is due.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised. The store is the one shared
//! engine, so what one request writes is what the next request reads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The region the tests sign for. Not the emulator's default, so the ARNs the handlers
/// mint are the ones the REQUEST named rather than a constant.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// How long a poll waits for a transition the clock owes before giving up. Generous: it
/// bounds a transition the emulator makes in seconds, so a loaded host still passes.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks. Short enough that the loop is cheap, and never
/// asserted on: every test waits for the CONDITION.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The statuses a dataset leaves on its own, in which "all writes are blocked" or a
/// removal is still owed (`smithy_query shape=DatasetStatus`).
const TRANSIENT_STATUSES: [&str; 3] = ["CREATING", "UPDATING", "DELETING"];

/// A `DatasetSchemaType` value, from `smithy_query shape=DatasetSchemaType`. Which schema
/// a dataset carries does not change what a delete removes, so one value serves every
/// case here; the schema catalog is covered by the create's own suites.
const SCHEMA_TYPE: &str = "AGENTCORE_EVALUATION_PREDEFINED_V1";

/// The sentence this service reports a `datasetId` naming no dataset in
/// (`crate::state::dataset_not_found`).
const DATASET_NOT_FOUND: &str =
    "The specified dataset does not exist. Please verify the datasetId and try again.";

/// The sentence this service reports a `datasetVersion` this dataset never published in
/// (`crate::state::resource_not_found` for the dataset-version noun).
const DATASET_VERSION_NOT_FOUND: &str = "Failed to retrieve dataset version because it \
                                         doesn't exist. Retry the request with a different \
                                         resource identifier.";

/// This emulator: the control plane over one shared engine, optionally with object
/// storage mounted beside it — which is what the assembled emulator is, and what an
/// Amazon S3 dataset source is ingested out of.
struct Emulator {
    router: MultiServiceRouter,
    object_storage: bool,
}

impl Emulator {
    fn new() -> Self {
        Self::over_engine(false)
    }

    fn with_object_storage() -> Self {
        Self::over_engine(true)
    }

    fn over_engine(object_storage: bool) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // `default()` registers no authenticator, so the suite exercises routing and the
        // handlers rather than this process's SigV4 keys.
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        if object_storage {
            router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
                Arc::clone(&engine),
            ));
        }
        Self {
            router,
            object_storage,
        }
    }

    /// One `restJson1` request to the control plane, signed for [`REGION`] — which is
    /// what the router routes on and what the handlers read the ARN Region from.
    async fn call(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261029/{REGION}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", authorization)
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// `CreateDataset`: `POST /datasets` (`smithy_query operation=CreateDataset`).
    async fn create_dataset(&self, name: &str, examples: usize) -> Answer {
        self.call(
            "POST",
            "/datasets",
            Some(&json!({
                "datasetName": name,
                "schemaType": SCHEMA_TYPE,
                "source": inline_source(examples),
            })),
        )
        .await
    }

    /// `GetDataset`: `GET /datasets/{datasetId}`.
    async fn get_dataset(&self, dataset_id: &str) -> Answer {
        self.call("GET", &format!("/datasets/{dataset_id}"), None)
            .await
    }

    /// `GetDataset` for one `datasetVersion`.
    async fn get_dataset_version(&self, dataset_id: &str, version: &str) -> Answer {
        self.call(
            "GET",
            &format!("/datasets/{dataset_id}?datasetVersion={version}"),
            None,
        )
        .await
    }

    /// `ListDatasets`: `GET /datasets`.
    async fn list_datasets(&self) -> Answer {
        self.call("GET", "/datasets", None).await
    }

    /// `ListDatasetVersions`: `GET /datasets/{datasetId}/versions`.
    async fn list_versions(&self, dataset_id: &str) -> Answer {
        self.call("GET", &format!("/datasets/{dataset_id}/versions"), None)
            .await
    }

    /// `CreateDatasetVersion`: `POST /datasets/{datasetId}/versions`.
    async fn publish_version(&self, dataset_id: &str) -> Answer {
        self.call(
            "POST",
            &format!("/datasets/{dataset_id}/versions"),
            Some(&json!({})),
        )
        .await
    }

    /// `AddDatasetExamples`: `POST /datasets/{datasetId}/examples/add`.
    async fn add_examples(&self, dataset_id: &str, examples: usize) -> Answer {
        self.call(
            "POST",
            &format!("/datasets/{dataset_id}/examples/add"),
            Some(&json!({"source": inline_source(examples)})),
        )
        .await
    }

    /// `DeleteDataset`: `DELETE /datasets/{datasetId}`, optionally naming one version
    /// (`smithy_query operation=DeleteDataset`).
    async fn delete_dataset(&self, dataset_id: &str, version: Option<&str>) -> Answer {
        let uri = match version {
            None => format!("/datasets/{dataset_id}"),
            Some(version) => format!("/datasets/{dataset_id}?datasetVersion={version}"),
        };
        self.call("DELETE", &uri, None).await
    }

    /// Create a dataset holding `examples` inline examples and wait for the clock to
    /// settle it `ACTIVE`, answering its `datasetId`.
    async fn settled_dataset(&self, case: &str, name: &str, examples: usize) -> String {
        let created = self.create_dataset(name, examples).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected HTTP 202 from CreateDataset, got {} and body {}",
            created.status, created.body
        );
        let dataset_id = created.text("datasetId");
        self.poll_until_settled(case, &dataset_id, "ACTIVE").await;
        dataset_id
    }

    /// Look at the dataset until it reports a status the clock has SETTLED it in — any
    /// status that is not one of [`TRANSIENT_STATUSES`] — and assert that status is
    /// `expected`. Waits for the CONDITION: it polls until the status holds or
    /// [`POLL_DEADLINE`] passes, so it never depends on how fast the host runs.
    async fn poll_until_settled(&self, case: &str, dataset_id: &str, expected: &str) -> Answer {
        let started = Instant::now();
        loop {
            let read = self.get_dataset(dataset_id).await;
            if !TRANSIENT_STATUSES.contains(&read.text("status").as_str()) {
                assert_eq!(
                    read.text("status"),
                    expected,
                    "case {case:?}: dataset {dataset_id} settled in {:?} rather than \
                     {expected:?}, body {}",
                    read.text("status"),
                    read.body
                );
                return read;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: dataset {dataset_id} never settled, last read was status {:?} \
                 and body {}",
                read.text("status"),
                read.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Look at the dataset until a read of it answers the modelled 404 — the removal the
    /// delete owed has come due — and answer that read. Waits for the CONDITION, bounded
    /// by [`POLL_DEADLINE`].
    async fn poll_until_gone(&self, case: &str, dataset_id: &str) -> Answer {
        let started = Instant::now();
        loop {
            let read = self.get_dataset(dataset_id).await;
            if read.status == 404 {
                return read;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: dataset {dataset_id} was still there, last read answered {} \
                 with body {}",
                read.status,
                read.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One path-style request to object storage's front door. Object storage is a REST
    /// service: the bucket and the key live in the URI and the body is the object's own
    /// bytes, so this is a raw request. The credential scope names `s3`, which is what
    /// the router claims on.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> u16 {
        assert!(
            self.object_storage,
            "an emulator built without object storage cannot be seeded with objects"
        );
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261029/{REGION}/s3/aws4_request, \
             SignedHeaders=host, Signature=abc"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", authorization)
            .body(Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        self.router.dispatch(request).await.status().as_u16()
    }

    /// Store `body` at `key` in `bucket`, creating the bucket first, so an ingestion that
    /// reads that location finds the file there.
    async fn put_object(&self, bucket: &str, key: &str, body: &[u8]) {
        let created = self
            .s3(
                "PUT",
                &format!("/{bucket}"),
                format!(
                    "<CreateBucketConfiguration \
                     xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                )
                .as_bytes(),
            )
            .await;
        assert!(
            created == 200 || created == 409,
            "seeding s3://{bucket}: expected 200 or 409 from CreateBucket, got {created}"
        );
        let stored = self.s3("PUT", &format!("/{bucket}/{key}"), body).await;
        assert_eq!(
            stored, 200,
            "seeding s3://{bucket}/{key}: expected 200 from PutObject, got {stored}"
        );
    }
}

/// One answer from the emulator: its status and its JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Self { status, body }
    }

    /// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn number(&self, member: &str) -> i64 {
        self.body
            .get(member)
            .and_then(Value::as_i64)
            .unwrap_or(i64::MIN)
    }

    /// The `datasetId`s a `ListDatasets` page reports.
    fn listed_dataset_ids(&self) -> Vec<String> {
        self.body
            .get("datasets")
            .and_then(Value::as_array)
            .map(|datasets| {
                datasets
                    .iter()
                    .map(|summary| {
                        summary
                            .get("datasetId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `datasetVersion`s a `ListDatasetVersions` page reports, in page order.
    fn listed_versions(&self) -> Vec<String> {
        self.body
            .get("versions")
            .and_then(Value::as_array)
            .map(|versions| {
                versions
                    .iter()
                    .map(|summary| {
                        summary
                            .get("datasetVersion")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A `source` carrying `count` inline examples, each a JSON object carrying the field the
/// AgentCore evaluation schemas require of one
/// (`crate::state::required_example_fields`).
fn inline_source(count: usize) -> Value {
    let examples: Vec<Value> = (0..count)
        .map(|index| {
            json!({
                "scenario_id": format!("scenario-{index}"),
                "input": format!("turn {index}"),
            })
        })
        .collect();
    json!({"inlineExamples": {"examples": examples}})
}

/// The ARN this service publishes a dataset under: `DatasetArn`'s pattern
/// `^arn:aws(-[a-z]+)*:bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:dataset/[a-zA-Z0-9_-]{1,110}$`.
fn dataset_arn(dataset_id: &str) -> String {
    format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:dataset/{dataset_id}")
}

/// Assert the five members `DeleteDatasetResponse` declares, for a delete that was
/// accepted: the dataset it addressed, the version it is removing, the status the dataset
/// reports from now on, and when the delete was initiated.
fn assert_accepted(case: &str, answer: &Answer, dataset_id: &str, version: &str) {
    assert_eq!(
        answer.status, 202,
        "case {case:?}: expected HTTP 202 from DeleteDataset, got {} and body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.text("datasetId"),
        dataset_id,
        "case {case:?}: expected the datasetId the request named, got {:?}",
        answer.text("datasetId")
    );
    assert_eq!(
        answer.text("datasetArn"),
        dataset_arn(dataset_id),
        "case {case:?}: expected the dataset's published ARN, got {:?}",
        answer.text("datasetArn")
    );
    assert_eq!(
        answer.text("datasetVersion"),
        version,
        "case {case:?}: expected the version being deleted, got {:?}",
        answer.text("datasetVersion")
    );
    assert_eq!(
        answer.text("status"),
        "DELETING",
        "case {case:?}: an accepted delete reports DELETING, got {:?} and body {}",
        answer.text("status"),
        answer.body
    );
    // `updatedAt` targets the prelude `Timestamp` with no `@timestampFormat` of its own
    // (`smithy_query operation=DeleteDataset`), which `restJson1` renders as epoch
    // seconds.
    let updated_at = answer.body.get("updatedAt").cloned().unwrap_or(Value::Null);
    let seconds = updated_at.as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: updatedAt must be an epoch-seconds number, got {updated_at:?}")
    });
    assert!(
        seconds > 0.0,
        "case {case:?}: updatedAt must name a real instant, got {seconds}"
    );
}

/// A delete that names no version removes the dataset record and the versions published
/// from it, and frees the name it reserved.
#[tokio::test]
async fn a_delete_naming_no_version_removes_the_whole_dataset() {
    let emulator = Emulator::new();
    let case = "the whole dataset";

    let dataset_id = emulator.settled_dataset(case, "doomed_set", 2).await;
    let published = emulator.publish_version(&dataset_id).await;
    assert_eq!(
        published.status, 202,
        "case {case:?}: expected HTTP 202 from CreateDatasetVersion, got {} and body {}",
        published.status, published.body
    );
    emulator.poll_until_settled(case, &dataset_id, "ACTIVE").await;

    // "If datasetVersion is absent, deletes all versions and the dataset record itself":
    // the working copy every dataset route resolves to when a request names no version is
    // what the response reports as deleted.
    let accepted = emulator.delete_dataset(&dataset_id, None).await;
    assert_accepted(case, &accepted, &dataset_id, "DRAFT");

    // The deletion is asynchronous, so the dataset is still there and reporting
    // `DELETING`, in which "read operations are still allowed".
    let during = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        during.status, 200,
        "case {case:?}: a dataset whose delete is under way is still readable, got {} and \
         body {}",
        during.status, during.body
    );
    assert_eq!(
        during.text("status"),
        "DELETING",
        "case {case:?}: expected DELETING while the removal is owed, got {:?} and body {}",
        during.text("status"),
        during.body
    );
    assert!(
        emulator
            .list_datasets()
            .await
            .listed_dataset_ids()
            .contains(&dataset_id),
        "case {case:?}: a dataset still going away is still listed, got {:?}",
        emulator.list_datasets().await.listed_dataset_ids()
    );

    // `@idempotent`: a repeat while the deletion is under way reports that same deletion.
    let repeat = emulator.delete_dataset(&dataset_id, None).await;
    assert_accepted("a repeated delete", &repeat, &dataset_id, "DRAFT");

    // Once the span the clock owed has passed, the record is gone: a read is the modelled
    // 404, the listing no longer reports it, and so is a read of its version history.
    let gone = emulator.poll_until_gone(case, &dataset_id).await;
    assert_eq!(
        gone.error_code(),
        "ResourceNotFoundException",
        "case {case:?}: expected the modelled ResourceNotFoundException, got {} and body {}",
        gone.error_code(),
        gone.body
    );
    assert_eq!(
        gone.text("message"),
        DATASET_NOT_FOUND,
        "case {case:?}: expected this service's missing-dataset sentence, got {:?}",
        gone.text("message")
    );
    assert!(
        !emulator
            .list_datasets()
            .await
            .listed_dataset_ids()
            .contains(&dataset_id),
        "case {case:?}: a removed dataset is listed by nothing, got {:?}",
        emulator.list_datasets().await.listed_dataset_ids()
    );
    let versions = emulator.list_versions(&dataset_id).await;
    assert_eq!(
        versions.status, 404,
        "case {case:?}: the version history of a removed dataset is the modelled 404, got {} \
         and body {}",
        versions.status, versions.body
    );

    // "Must be unique within the account" bound the name to the dataset that held it, so
    // the name is free again once the record is gone.
    let recreated = emulator.create_dataset("doomed_set", 1).await;
    assert_eq!(
        recreated.status, 202,
        "case {case:?}: expected HTTP 202 re-creating under the freed name, got {} and body {}",
        recreated.status, recreated.body
    );
    assert_ne!(
        recreated.text("datasetId"),
        dataset_id,
        "case {case:?}: the re-created dataset carries an id of its own, got {:?}",
        recreated.text("datasetId")
    );
}

/// One delete that names a version: which version it names, and what the dataset holds
/// afterwards.
struct VersionRow {
    case: &'static str,
    name: &'static str,
    /// The `datasetVersion` the delete carries.
    label: &'static str,
    /// The published versions the dataset still holds once the removal is due.
    remaining: &'static [&'static str],
    /// How many examples the DRAFT working copy holds afterwards.
    draft_examples: i64,
}

/// A delete that names a version removes ONLY that version: the dataset record, its other
/// versions and the examples each of them holds are left exactly as they were.
///
/// The rows differ along what the rule generalises over: which of a dataset's versions is
/// named — the oldest published, the latest published, and the `DRAFT` working copy, which
/// exists for as long as the dataset record does and so loses its examples rather than
/// itself.
#[tokio::test]
async fn a_delete_naming_a_version_removes_only_that_version() {
    let emulator = Emulator::new();

    /// How many examples every dataset in the table is created with, so a version holds a
    /// count a read can tell from an empty one.
    const EXAMPLES: usize = 2;
    /// How many versions each dataset publishes before one of them is deleted.
    const PUBLISHED: usize = 3;

    let rows: [VersionRow; 3] = [
        VersionRow {
            case: "the oldest published version",
            name: "oldest_set",
            label: "1",
            remaining: &["2", "3"],
            draft_examples: EXAMPLES as i64,
        },
        VersionRow {
            case: "the latest published version",
            name: "latest_set",
            label: "3",
            remaining: &["1", "2"],
            draft_examples: EXAMPLES as i64,
        },
        VersionRow {
            case: "the DRAFT working copy",
            name: "draft_set",
            label: "DRAFT",
            remaining: &["1", "2", "3"],
            draft_examples: 0,
        },
    ];

    // Every row's dataset is created first and then published from, so the spans the
    // clock owes them run side by side rather than one row at a time.
    let mut datasets: Vec<(&VersionRow, String)> = Vec::new();
    for row in &rows {
        let created = emulator.create_dataset(row.name, EXAMPLES).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: expected HTTP 202 from CreateDataset, got {} and body {}",
            row.case, created.status, created.body
        );
        datasets.push((row, created.text("datasetId")));
    }
    for (row, dataset_id) in &datasets {
        emulator
            .poll_until_settled(row.case, dataset_id, "ACTIVE")
            .await;
    }
    for round in 1..=PUBLISHED {
        for (row, dataset_id) in &datasets {
            let published = emulator.publish_version(dataset_id).await;
            assert_eq!(
                published.status, 202,
                "case {:?}: expected HTTP 202 publishing version {round}, got {} and body {}",
                row.case, published.status, published.body
            );
            assert_eq!(
                published.text("datasetVersion"),
                round.to_string(),
                "case {:?}: versions are numbered in publication order, got {:?}",
                row.case,
                published.text("datasetVersion")
            );
        }
        for (row, dataset_id) in &datasets {
            emulator
                .poll_until_settled(row.case, dataset_id, "ACTIVE")
                .await;
        }
    }

    for (row, dataset_id) in &datasets {
        let case = row.case;
        let accepted = emulator.delete_dataset(dataset_id, Some(row.label)).await;
        assert_accepted(case, &accepted, dataset_id, row.label);

        // The version is still readable while its removal is only owed, and the dataset
        // itself is reporting the deletion under way.
        let during = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            during.text("status"),
            "DELETING",
            "case {case:?}: expected DELETING while the removal is owed, got {:?} and body {}",
            during.text("status"),
            during.body
        );
    }

    // Once the owed span has passed the dataset is `ACTIVE` again — only the version it
    // was asked to delete is gone.
    for (row, dataset_id) in &datasets {
        let case = row.case;
        emulator.poll_until_settled(case, dataset_id, "ACTIVE").await;

        let listed = emulator.list_versions(dataset_id).await;
        assert_eq!(
            listed.listed_versions(),
            row.remaining
                .iter()
                .map(|version| (*version).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the versions the delete left, got {:?}",
            listed.listed_versions()
        );

        let draft = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            draft.status, 200,
            "case {case:?}: the dataset record outlives a version delete, got {} and body {}",
            draft.status, draft.body
        );
        assert_eq!(
            draft.number("exampleCount"),
            row.draft_examples,
            "case {case:?}: expected the DRAFT to hold {} examples, got {}",
            row.draft_examples,
            draft.body["exampleCount"]
        );

        // A read of the version the delete removed is the modelled 404; a version it left
        // still reports the examples it snapshotted.
        if row.label == "DRAFT" {
            continue;
        }
        let removed = emulator.get_dataset_version(dataset_id, row.label).await;
        assert_eq!(
            removed.status, 404,
            "case {case:?}: a read of the deleted version is the modelled 404, got {} and \
             body {}",
            removed.status, removed.body
        );
        assert_eq!(
            removed.text("message"),
            DATASET_VERSION_NOT_FOUND,
            "case {case:?}: expected this service's missing-version sentence, got {:?}",
            removed.text("message")
        );
        for kept in row.remaining {
            let read = emulator.get_dataset_version(dataset_id, kept).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: version {kept} was not deleted, got {} and body {}",
                read.status, read.body
            );
            assert_eq!(
                read.number("exampleCount"),
                EXAMPLES as i64,
                "case {case:?}: version {kept} still holds the examples it snapshotted, got {}",
                read.body["exampleCount"]
            );
        }
    }

    // A version number addresses one snapshot for good: the next publication carries the
    // next number rather than re-using the one the delete freed.
    for (row, dataset_id) in &datasets {
        let published = emulator.publish_version(dataset_id).await;
        assert_eq!(
            published.text("datasetVersion"),
            (PUBLISHED + 1).to_string(),
            "case {:?}: a deleted version's number is not re-used, got {:?}",
            row.case,
            published.text("datasetVersion")
        );
    }
}

/// While a delete is under way the dataset is `DELETING`, in which every write of it is
/// refused with the modelled conflict — and a dataset busy with another operation refuses
/// the delete itself for the same reason.
#[tokio::test]
async fn writes_conflict_while_a_delete_is_under_way() {
    let emulator = Emulator::new();

    // A dataset whose own ingestion is still in flight is `CREATING`, in which "all
    // writes are blocked" — and a delete is a write.
    let creating = emulator.create_dataset("busy_set", 2).await;
    let busy_id = creating.text("datasetId");
    let refused = emulator.delete_dataset(&busy_id, None).await;
    assert_eq!(
        refused.status, 409,
        "case \"CREATING\": expected HTTP 409 deleting a creating dataset, got {} and body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ConflictException",
        "case \"CREATING\": expected the modelled ConflictException, got {} and body {}",
        refused.error_code(),
        refused.body
    );
    assert_eq!(
        refused.text("message"),
        "Dataset is not in a state that allows this operation: CREATING",
        "case \"CREATING\": the refusal names the state the dataset is in, got {:?}",
        refused.text("message")
    );
    emulator
        .poll_until_settled("CREATING", &busy_id, "ACTIVE")
        .await;

    let case = "DELETING";
    let accepted = emulator.delete_dataset(&busy_id, None).await;
    assert_accepted(case, &accepted, &busy_id, "DRAFT");

    // Every write of a dataset on its way out conflicts with the deletion already under
    // way on it, in the one sentence this service words a blocked write in.
    let add = emulator.add_examples(&busy_id, 1).await;
    assert_eq!(
        add.status, 409,
        "case {case:?}: expected HTTP 409 adding examples to a deleting dataset, got {} and \
         body {}",
        add.status, add.body
    );
    assert_eq!(
        add.text("message"),
        "Dataset is not in a state that allows this operation: DELETING",
        "case {case:?}: the refusal names the state the dataset is in, got {:?}",
        add.text("message")
    );
    let publish = emulator.publish_version(&busy_id).await;
    assert_eq!(
        publish.status, 409,
        "case {case:?}: expected HTTP 409 publishing from a deleting dataset, got {} and \
         body {}",
        publish.status, publish.body
    );
    assert_eq!(
        publish.error_code(),
        "ConflictException",
        "case {case:?}: expected the modelled ConflictException, got {} and body {}",
        publish.error_code(),
        publish.body
    );

    // The dataset goes away, so nothing it held was written by the refused calls.
    emulator.poll_until_gone(case, &busy_id).await;
}

/// One delete this service refuses, and how.
struct RefusalRow {
    case: &'static str,
    /// Whether the request names the dataset the test created, or an id naming none.
    known_dataset: bool,
    /// The `datasetVersion` the request carries, if any.
    version: Option<&'static str>,
    status: u16,
    error_code: &'static str,
    message: &'static str,
}

/// A delete that names no dataset, or a version the dataset never published, is refused —
/// and a `datasetVersion` outside the member's own pattern never reaches the handler.
///
/// The rows differ along which part of the request is wrong: the `datasetId`, the
/// `datasetVersion` as a value the dataset does not hold, and the `datasetVersion` as a
/// spelling the model does not admit.
#[tokio::test]
async fn a_delete_naming_no_dataset_or_version_is_refused() {
    let emulator = Emulator::new();

    // A dataset holding exactly one published version, so a request naming a higher
    // number names a version this dataset never published.
    let dataset_id = emulator.settled_dataset("setup", "kept_set", 1).await;
    let published = emulator.publish_version(&dataset_id).await;
    assert_eq!(
        published.status, 202,
        "case \"setup\": expected HTTP 202 from CreateDatasetVersion, got {} and body {}",
        published.status, published.body
    );
    emulator
        .poll_until_settled("setup", &dataset_id, "ACTIVE")
        .await;

    let rows: [RefusalRow; 5] = [
        RefusalRow {
            case: "a datasetId naming no dataset",
            known_dataset: false,
            version: None,
            status: 404,
            error_code: "ResourceNotFoundException",
            message: DATASET_NOT_FOUND,
        },
        RefusalRow {
            case: "a version of a dataset that is not there",
            known_dataset: false,
            version: Some("1"),
            status: 404,
            error_code: "ResourceNotFoundException",
            message: DATASET_NOT_FOUND,
        },
        RefusalRow {
            case: "a version number this dataset never published",
            known_dataset: true,
            version: Some("2"),
            status: 404,
            error_code: "ResourceNotFoundException",
            message: DATASET_VERSION_NOT_FOUND,
        },
        RefusalRow {
            case: "a version number far past the ones published",
            known_dataset: true,
            version: Some("4294967295"),
            status: 404,
            error_code: "ResourceNotFoundException",
            message: DATASET_VERSION_NOT_FOUND,
        },
        RefusalRow {
            // `DatasetVersion` is `^(DRAFT|[0-9]+)$` (`smithy_query shape=DatasetVersion`),
            // so a spelling outside it is refused by the boundary before any handler runs.
            case: "a datasetVersion outside the modelled pattern",
            known_dataset: true,
            version: Some("latest"),
            status: 400,
            error_code: "ValidationException",
            message: "",
        },
    ];

    for row in &rows {
        let case = row.case;
        let addressed = if row.known_dataset {
            dataset_id.clone()
        } else {
            "no_such_dataset".to_string()
        };
        let refused = emulator.delete_dataset(&addressed, row.version).await;
        assert_eq!(
            refused.status, row.status,
            "case {case:?}: expected HTTP {}, got {} and body {}",
            row.status, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            row.error_code,
            "case {case:?}: expected {}, got {} and body {}",
            row.error_code,
            refused.error_code(),
            refused.body
        );
        if !row.message.is_empty() {
            assert_eq!(
                refused.text("message"),
                row.message,
                "case {case:?}: expected this service's refusal sentence, got {:?}",
                refused.text("message")
            );
        }
    }

    // Nothing of a refused delete was written: the dataset and its version are both still
    // there, and the dataset is not reporting a deletion under way.
    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.text("status"),
        "ACTIVE",
        "case \"after the refusals\": a refused delete starts nothing, got {:?} and body {}",
        read.text("status"),
        read.body
    );
    assert_eq!(
        emulator.list_versions(&dataset_id).await.listed_versions(),
        vec!["1".to_string()],
        "case \"after the refusals\": a refused delete removed no version, got {:?}",
        emulator.list_versions(&dataset_id).await.listed_versions()
    );
}

/// Which deletion a request asks for is decided by its SHAPE: a request carrying no
/// `datasetVersion` asks for the whole record, which only that record's own deletion
/// answers for, while a request naming a version is answered by a whole-dataset deletion
/// already under way, because that one takes every version with it.
#[tokio::test]
async fn a_delete_answers_only_for_the_deletion_that_was_asked_for() {
    let emulator = Emulator::new();

    // A deletion of the DRAFT working copy is accepted first.
    let case = "the DRAFT first";
    let drafted = emulator.settled_dataset(case, "draft_first_set", 2).await;
    let draft_delete = emulator.delete_dataset(&drafted, Some("DRAFT")).await;
    assert_accepted(case, &draft_delete, &drafted, "DRAFT");

    // The whole-dataset delete is a deletion nothing has accepted, so it is refused as
    // the conflict it is rather than answered by the version delete already running —
    // which would report a removal that was never armed.
    let whole = emulator.delete_dataset(&drafted, None).await;
    assert_eq!(
        whole.status, 409,
        "case {case:?}: expected HTTP 409 deleting a dataset already DELETING, got {} and \
         body {}",
        whole.status, whole.body
    );
    assert_eq!(
        whole.error_code(),
        "ConflictException",
        "case {case:?}: expected the modelled ConflictException, got {} and body {}",
        whole.error_code(),
        whole.body
    );
    assert_eq!(
        whole.text("message"),
        "Dataset is not in a state that allows this operation: DELETING",
        "case {case:?}: the refusal names the state the dataset is in, got {:?}",
        whole.text("message")
    );

    // Once that version delete has settled, the whole-dataset delete is accepted and the
    // record really goes.
    emulator.poll_until_settled(case, &drafted, "ACTIVE").await;
    let whole = emulator.delete_dataset(&drafted, None).await;
    assert_accepted(case, &whole, &drafted, "DRAFT");
    emulator.poll_until_gone(case, &drafted).await;

    // The other order: while the WHOLE dataset is going away, a request naming one of its
    // versions is answered by that deletion, and the record is still removed on time.
    let case = "the whole dataset first";
    let doomed = emulator.settled_dataset(case, "whole_first_set", 2).await;
    let published = emulator.publish_version(&doomed).await;
    assert_eq!(
        published.status, 202,
        "case {case:?}: expected HTTP 202 from CreateDatasetVersion, got {} and body {}",
        published.status, published.body
    );
    emulator.poll_until_settled(case, &doomed, "ACTIVE").await;

    let whole = emulator.delete_dataset(&doomed, None).await;
    assert_accepted(case, &whole, &doomed, "DRAFT");
    for version in ["DRAFT", "1"] {
        let named = emulator.delete_dataset(&doomed, Some(version)).await;
        assert_accepted(case, &named, &doomed, version);
    }
    emulator.poll_until_gone(case, &doomed).await;
}

/// The bucket this suite's JSONL file lives in, inside `S3Uri`'s pattern
/// `^s3://[a-z0-9][a-z0-9.\-]{1,61}[a-z0-9]/.{1,1024}$` (`smithy_query shape=S3Source`).
const BUCKET: &str = "agentcore-deleted-datasets";

/// A version delete removes the version and nothing else: the status the dataset was
/// already settled in — here the `CREATE_FAILED` of an ingestion that yielded nothing,
/// with the reason it reports — is what it reports again once the delete settles, rather
/// than a failure an unrelated delete resurrected as `ACTIVE`.
#[tokio::test]
async fn a_version_delete_leaves_a_failed_dataset_in_its_failure() {
    let emulator = Emulator::with_object_storage();
    let case = "a dataset whose ingestion failed";

    // A file that resolves but holds no example: "initial ingestion failed. DRAFT record
    // exists but contains no examples" (`smithy_query shape=DatasetStatus`).
    let key = "failed/examples.jsonl";
    emulator.put_object(BUCKET, key, b"\n  \n").await;
    let created = emulator
        .call(
            "POST",
            "/datasets",
            Some(&json!({
                "datasetName": "failed_set",
                "schemaType": SCHEMA_TYPE,
                "source": {"s3Source": {"s3Uri": format!("s3://{BUCKET}/{key}")}},
            })),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202 from CreateDataset, got {} and body {}",
        created.status, created.body
    );
    let dataset_id = created.text("datasetId");
    let failed = emulator
        .poll_until_settled(case, &dataset_id, "CREATE_FAILED")
        .await;
    let reason = failed.text("failureReason");
    assert!(
        !reason.is_empty(),
        "case {case:?}: a failed ingestion reports why, got body {}",
        failed.body
    );

    // A failed ingestion blocks no write, so the delete of its working copy is accepted.
    let accepted = emulator.delete_dataset(&dataset_id, Some("DRAFT")).await;
    assert_accepted(case, &accepted, &dataset_id, "DRAFT");

    let settled = emulator
        .poll_until_settled(case, &dataset_id, "CREATE_FAILED")
        .await;
    assert_eq!(
        settled.text("failureReason"),
        reason,
        "case {case:?}: the delete of a version changes nothing about why the ingestion \
         failed, got {:?}",
        settled.text("failureReason")
    );
    assert_eq!(
        settled.number("exampleCount"),
        0,
        "case {case:?}: the working copy holds nothing either way, got {}",
        settled.body["exampleCount"]
    );
}
