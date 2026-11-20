// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `AddDatasetExamples`.
//!
//! The rule under test: an add appends the batch its `source` yields to the dataset's
//! DRAFT working copy, under one freshly minted `ExampleId` per example, reports that
//! count and those ids, and leaves the dataset `UPDATING` until the clock settles it —
//! refusing the whole batch when one example fails the DATASET's schema, refusing a write
//! while the dataset is in a status that blocks writes, and refusing a `datasetId` naming
//! no dataset.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised. The store is the one shared
//! engine, so what one request writes is what the next request reads.
//!
//! The suite that adds examples out of a JSONL file mounts object storage beside this
//! service on ONE engine — what the assembled emulator does — and stores the file through
//! object storage's own front door, so the examples the add counts are the ones object
//! storage really holds.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The region the tests sign for. Not the emulator's default, so the ARNs the handlers
/// mint and the object-storage endpoint an ingestion reads at are the ones the REQUEST
/// named rather than a constant.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// How long a poll waits for a status the clock owes before giving up. Generous: it
/// bounds a transition the emulator makes in seconds, so a loaded host still passes.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks. Short enough that the loop is cheap, and never
/// asserted on: the test waits for the CONDITION.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Every `DatasetSchemaType` value, from `smithy_query shape=DatasetSchemaType`.
const SCHEMA_TYPES: [&str; 3] = [
    "AGENTCORE_EVALUATION_PREDEFINED_V1",
    "AGENTCORE_EVALUATION_SIMULATED_V1",
    "THIRD_PARTY_EVALUATION_V1",
];

/// The transient statuses a dataset leaves on its own, in which "all writes are blocked"
/// (`smithy_query shape=DatasetStatus`).
const TRANSIENT_STATUSES: [&str; 2] = ["CREATING", "UPDATING"];

/// How many examples one dataset holds in this emulator (`crate::state`'s
/// `MAX_DATASET_EXAMPLES`), which is also `DatasetExampleList`'s own per-request ceiling
/// (`length 1..1000`, `smithy_query shape=InlineExamplesSource`).
const MAX_EXAMPLES: usize = 1_000;

/// A `clientToken` inside the modelled `ClientToken` constraints (len 33..256,
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`, `smithy_query operation=AddDatasetExamples`).
fn client_token(seed: &str) -> String {
    let token = format!("{seed}{}", "0".repeat(40));
    token[..40].to_string()
}

/// This emulator: the control plane, optionally with object storage mounted beside it on
/// the one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    object_storage: bool,
}

impl Emulator {
    /// Only the control plane, which every inline source is served by.
    fn new() -> Self {
        Self::over_engine(false)
    }

    /// The control plane with object storage answering, which is what the assembled
    /// emulator is.
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
    async fn create_dataset(&self, body: &Value) -> Answer {
        self.call("POST", "/datasets", Some(body)).await
    }

    /// `GetDataset`: `GET /datasets/{datasetId}`.
    async fn get_dataset(&self, dataset_id: &str) -> Answer {
        self.call("GET", &format!("/datasets/{dataset_id}"), None)
            .await
    }

    /// `AddDatasetExamples`: `POST /datasets/{datasetId}/examples/add`
    /// (`smithy_query operation=AddDatasetExamples`).
    async fn add_examples(&self, dataset_id: &str, body: &Value) -> Answer {
        self.call(
            "POST",
            &format!("/datasets/{dataset_id}/examples/add"),
            Some(body),
        )
        .await
    }

    /// Create a dataset holding `examples` inline examples of `schema_type`, wait for the
    /// clock to settle it, and answer its `datasetId`.
    async fn settled_dataset(
        &self,
        case: &str,
        name: &str,
        schema_type: &str,
        examples: usize,
    ) -> String {
        let created = self
            .create_dataset(&json!({
                "datasetName": name,
                "schemaType": schema_type,
                "source": inline_source(examples),
            }))
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected HTTP 202 from CreateDataset, got {} and body {}",
            created.status, created.body
        );
        let dataset_id = created.text("datasetId");
        let read = self.poll_until_settled(case, &dataset_id).await;
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: an ingested dataset settles ACTIVE, got {} and body {}",
            read.text("status"),
            read.body
        );
        dataset_id
    }

    /// Look at the dataset until it reports a status the clock has SETTLED it in — any
    /// status that is not one of [`TRANSIENT_STATUSES`] — and answer that read. Waits for
    /// the CONDITION: it polls until the status holds or [`POLL_DEADLINE`] passes, so it
    /// never depends on how fast the host runs.
    async fn poll_until_settled(&self, case: &str, dataset_id: &str) -> Answer {
        let started = Instant::now();
        loop {
            let read = self.get_dataset(dataset_id).await;
            if !TRANSIENT_STATUSES.contains(&read.text("status").as_str()) {
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

    /// Store `body` at `key` in `bucket`, creating the bucket first, so an add that reads
    /// that location finds the file there.
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

    /// The `exampleIds` an add answered.
    fn example_ids(&self) -> Vec<String> {
        self.body
            .get("exampleIds")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The field an example of `schema_type` must carry
/// (`crate::state::required_example_fields`): both AgentCore evaluation schemas address
/// an example as a scenario, and the third-party schema's example IS its input
/// (`smithy_query doc shape=DatasetSchemaType`).
fn required_field(schema_type: &str) -> &'static str {
    if schema_type == SCHEMA_TYPES[2] {
        "input"
    } else {
        "scenario_id"
    }
}

/// One example carrying the fields both AgentCore evaluation schemas and the third-party
/// schema require, so the same batch satisfies every `DatasetSchemaType`.
fn example(index: usize) -> Value {
    json!({
        "scenario_id": format!("scenario-{index}"),
        "input": format!("turn {index}"),
        "expected": {"score": index},
    })
}

/// A `source` carrying `count` inline examples, each satisfying every schema type. An
/// example is a JSON object, which is what `DatasetExampleList`'s `document` member
/// admits (`smithy_query shape=DatasetExampleList`).
fn inline_source(count: usize) -> Value {
    let examples: Vec<Value> = (0..count).map(example).collect();
    json!({"inlineExamples": {"examples": examples}})
}

/// An `ExampleId`'s pattern, `^[a-zA-Z0-9_.:-]+$` with `length 0..256` (`smithy_query
/// shape=ExampleIdList`), checked by hand because this crate carries no regex engine.
fn is_modelled_example_id(id: &str) -> bool {
    (1..=256).contains(&id.len())
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' || byte == b':'
                || byte == b'-'
        })
}

/// Assert one `Timestamp` member's wire form: an epoch-seconds NUMBER naming a real
/// instant. `updatedAt` targets the prelude `Timestamp` with no `@timestampFormat` of its
/// own (`smithy_query operation=AddDatasetExamples`), which `restJson1` renders as epoch
/// seconds.
fn assert_epoch_seconds(case: &str, member: &str, value: &Value) {
    let seconds = value
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be an epoch-seconds number, got {value:?}"));
    assert!(
        seconds > 0.0,
        "case {case:?}: {member} must name a real instant, got {seconds}"
    );
}

/// Assert the ids an add answered are well-formed, as many as the batch held, and all
/// distinct from each other and from `already_held`.
fn assert_minted_ids(case: &str, ids: &[String], added: usize, already_held: &BTreeSet<String>) {
    assert_eq!(
        ids.len(),
        added,
        "case {case:?}: expected one exampleId per added example ({added}), got {ids:?}"
    );
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "case {case:?}: every added example gets its own id, got {ids:?}"
    );
    for id in ids {
        assert!(
            is_modelled_example_id(id),
            "case {case:?}: expected an ExampleId matching ^[a-zA-Z0-9_.:-]+$, got {id:?}"
        );
        assert!(
            !already_held.contains(id),
            "case {case:?}: an id already held by this dataset was minted again: {id:?}"
        );
    }
}

/// One add of an inline batch: the dataset it grows, and what the add must report.
struct AddRow {
    case: &'static str,
    name: &'static str,
    schema_type: &'static str,
    /// How many examples the create's source yielded.
    initial: usize,
    /// How many the add's source carries.
    added: usize,
}

/// A batch is appended to the DRAFT working copy and read back through `GetDataset`, for
/// every shape of batch and schema the add accepts.
///
/// The rows differ along what the rule generalises over: the dataset's schema type (every
/// `DatasetSchemaType` value), how many examples the dataset already held, and how many
/// the batch carries — one, several, and a batch that fills the working copy to its
/// ceiling.
#[tokio::test]
async fn a_batch_is_appended_to_the_draft_and_read_back_for_every_schema_and_size() {
    let emulator = Emulator::new();

    let rows: [AddRow; 4] = [
        AddRow {
            case: "a single example added to a predefined dataset",
            name: "predefined_set",
            schema_type: SCHEMA_TYPES[0],
            initial: 1,
            added: 1,
        },
        AddRow {
            case: "several examples added to a simulated dataset",
            name: "simulated_set",
            schema_type: SCHEMA_TYPES[1],
            initial: 3,
            added: 2,
        },
        AddRow {
            case: "several examples added to a third-party dataset",
            name: "third_party_set",
            schema_type: SCHEMA_TYPES[2],
            initial: 2,
            added: 5,
        },
        AddRow {
            case: "a batch that fills the working copy to its ceiling",
            name: "full_set",
            schema_type: SCHEMA_TYPES[0],
            initial: 1,
            added: MAX_EXAMPLES - 1,
        },
    ];

    // The datasets the table creates, so the clock-owed transition is waited out ONCE per
    // phase rather than per row.
    let mut created: Vec<(&AddRow, String)> = Vec::new();
    for row in &rows {
        let answer = emulator
            .create_dataset(&json!({
                "datasetName": row.name,
                "schemaType": row.schema_type,
                "source": inline_source(row.initial),
            }))
            .await;
        assert_eq!(
            answer.status, 202,
            "case {:?}: expected HTTP 202 from CreateDataset, got {} and body {}",
            row.case, answer.status, answer.body
        );
        created.push((row, answer.text("datasetId")));
    }

    for (row, dataset_id) in &created {
        let read = emulator.poll_until_settled(row.case, dataset_id).await;
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {:?}: an ingested dataset settles ACTIVE, got {} and body {}",
            row.case,
            read.text("status"),
            read.body
        );
    }

    let mut added: Vec<(&AddRow, String, Vec<String>)> = Vec::new();
    for (row, dataset_id) in &created {
        let case = row.case;
        let answer = emulator
            .add_examples(dataset_id, &json!({"source": inline_source(row.added)}))
            .await;
        assert_eq!(
            answer.status, 202,
            "case {case:?}: expected HTTP 202 from AddDatasetExamples, got {} and body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.text("datasetId"),
            *dataset_id,
            "case {case:?}: the add reports the dataset it grew, got {}",
            answer.text("datasetId")
        );
        assert_eq!(
            answer.text("datasetArn"),
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:dataset/{dataset_id}"),
            "case {case:?}: expected the modelled DatasetArn for {dataset_id}, got {}",
            answer.text("datasetArn")
        );
        assert_eq!(
            answer.number("addedCount"),
            i64::try_from(row.added).expect("the batch size fits a Long"),
            "case {case:?}: expected {} examples added, got {}",
            row.added,
            answer.body["addedCount"]
        );
        assert_eq!(
            answer.text("status"),
            "UPDATING",
            "case {case:?}: an example mutation in progress reports UPDATING, got {} and \
             body {}",
            answer.text("status"),
            answer.body
        );
        assert_epoch_seconds(case, "updatedAt", &answer.body["updatedAt"]);
        let ids = answer.example_ids();
        assert_minted_ids(case, &ids, row.added, &BTreeSet::new());

        // The working copy holds what it held plus the batch, which the dataset reports
        // straight away: the examples are written by the call, and only the STATUS is
        // owed to the clock.
        let read = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            read.number("exampleCount"),
            i64::try_from(row.initial + row.added).expect("the example count fits a Long"),
            "case {case:?}: expected {} examples in the draft after the add, got {}",
            row.initial + row.added,
            read.body["exampleCount"]
        );
        assert_eq!(
            read.text("status"),
            "UPDATING",
            "case {case:?}: the dataset is UPDATING until the clock settles it, got {}",
            read.text("status")
        );
        assert_eq!(
            read.text("draftStatus"),
            "MODIFIED",
            "case {case:?}: a draft the add changed reflects no published version, got {}",
            read.text("draftStatus")
        );
        added.push((*row, dataset_id.clone(), ids));
    }

    for (row, dataset_id, ids) in &added {
        let case = row.case;
        let read = emulator.poll_until_settled(case, dataset_id).await;
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: a dataset the add wrote settles ACTIVE, got {} and body {}",
            read.text("status"),
            read.body
        );
        assert_eq!(
            read.number("exampleCount"),
            i64::try_from(row.initial + row.added).expect("the example count fits a Long"),
            "case {case:?}: the settled dataset still holds the added examples, got {}",
            read.body["exampleCount"]
        );
        assert!(
            read.body["failureReason"].is_null(),
            "case {case:?}: an add that wrote its batch reports no failureReason, got {}",
            read.body["failureReason"]
        );

        // A second add grows the same working copy again, under ids none of the examples
        // already in it hold.
        let already_held: BTreeSet<String> = ids.iter().cloned().collect();
        let again = emulator
            .add_examples(dataset_id, &json!({"source": inline_source(1)}))
            .await;
        let expected = if row.initial + row.added < MAX_EXAMPLES {
            202
        } else {
            // The working copy is full, so the add meets the modelled quota rather than
            // growing this emulator's store without bound.
            402
        };
        assert_eq!(
            again.status, expected,
            "case {case:?}: expected HTTP {expected} from a second add, got {} and body {}",
            again.status, again.body
        );
        if expected == 202 {
            assert_minted_ids(case, &again.example_ids(), 1, &already_held);
            let read = emulator.get_dataset(dataset_id).await;
            assert_eq!(
                read.number("exampleCount"),
                i64::try_from(row.initial + row.added + 1).expect("the count fits a Long"),
                "case {case:?}: the second add appended to the draft, got {}",
                read.body["exampleCount"]
            );
        } else {
            assert_eq!(
                again.error_code(),
                "ServiceQuotaExceededException",
                "case {case:?}: a full working copy is the modelled quota refusal, got {} \
                 and body {}",
                again.error_code(),
                again.body
            );
            let read = emulator.get_dataset(dataset_id).await;
            assert_eq!(
                read.number("exampleCount"),
                i64::try_from(MAX_EXAMPLES).expect("the ceiling fits a Long"),
                "case {case:?}: a refused add wrote nothing, got {}",
                read.body["exampleCount"]
            );
        }
    }
}

/// A dataset in a status that blocks writes refuses the add with the modelled conflict,
/// and accepts one again once the clock has settled it.
///
/// The rows are the transient statuses the model says block every write: the `CREATING`
/// of an ingestion still in flight and the `UPDATING` of an example mutation still in
/// flight.
#[tokio::test]
async fn an_add_is_refused_while_the_dataset_blocks_writes() {
    let emulator = Emulator::new();

    let created = emulator
        .create_dataset(&json!({
            "datasetName": "busy_set",
            "schemaType": SCHEMA_TYPES[0],
            "source": inline_source(2),
        }))
        .await;
    let dataset_id = created.text("datasetId");

    // A dataset whose own ingestion is still in flight is `CREATING`, in which "all
    // writes are blocked".
    let during_create = emulator
        .add_examples(&dataset_id, &json!({"source": inline_source(1)}))
        .await;
    assert_eq!(
        during_create.status, 409,
        "case \"CREATING\": expected HTTP 409 from an add to a creating dataset, got {} and \
         body {}",
        during_create.status, during_create.body
    );
    assert_eq!(
        during_create.error_code(),
        "ConflictException",
        "case \"CREATING\": expected the modelled ConflictException, got {} and body {}",
        during_create.error_code(),
        during_create.body
    );
    assert_eq!(
        during_create.text("message"),
        "Dataset is not in a state that allows this operation: CREATING",
        "case \"CREATING\": the refusal names the state the dataset is in, got {:?}",
        during_create.text("message")
    );

    emulator.poll_until_settled("CREATING", &dataset_id).await;

    let first = emulator
        .add_examples(&dataset_id, &json!({"source": inline_source(3)}))
        .await;
    assert_eq!(
        first.status, 202,
        "case \"UPDATING\": expected HTTP 202 from the first add, got {} and body {}",
        first.status, first.body
    );

    // The mutation that add started is in flight, so the dataset is `UPDATING` and the
    // next write conflicts with it rather than racing it.
    let during_update = emulator
        .add_examples(&dataset_id, &json!({"source": inline_source(1)}))
        .await;
    assert_eq!(
        during_update.status, 409,
        "case \"UPDATING\": expected HTTP 409 from a concurrent add, got {} and body {}",
        during_update.status, during_update.body
    );
    assert_eq!(
        during_update.error_code(),
        "ConflictException",
        "case \"UPDATING\": expected the modelled ConflictException, got {} and body {}",
        during_update.error_code(),
        during_update.body
    );
    assert_eq!(
        during_update.text("message"),
        "Dataset is not in a state that allows this operation: UPDATING",
        "case \"UPDATING\": the refusal names the state the dataset is in, got {:?}",
        during_update.text("message")
    );

    let refused = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        refused.number("exampleCount"),
        5,
        "case \"UPDATING\": the refused add wrote nothing, got {}",
        refused.body["exampleCount"]
    );

    // The conflict is transient: once the clock settles the dataset, the same add is
    // served.
    emulator.poll_until_settled("UPDATING", &dataset_id).await;
    let after_settle = emulator
        .add_examples(&dataset_id, &json!({"source": inline_source(1)}))
        .await;
    assert_eq!(
        after_settle.status, 202,
        "case \"settled\": expected HTTP 202 from an add to a settled dataset, got {} and \
         body {}",
        after_settle.status, after_settle.body
    );
    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        6,
        "case \"settled\": the add appended its example, got {}",
        read.body["exampleCount"]
    );
}

/// One batch that does not satisfy the dataset's schema: which example of it is wrong,
/// and how.
struct SchemaRow {
    case: &'static str,
    name: &'static str,
    schema_type: &'static str,
    /// The zero-based position of the offending example in the batch.
    index: usize,
    /// That example, as the request carries it.
    offending: Value,
}

/// A batch holding one example that fails the DATASET's schema is rejected whole, and
/// nothing of it is written.
///
/// The rows differ along what the rule generalises over: the schema type (and so the
/// field its examples must carry), where in the batch the offending example sits, and the
/// JSON form that makes it offend — an object that omits the field, and a value that is
/// not an object at all and so carries no field of the schema's structure.
#[tokio::test]
async fn a_batch_failing_the_dataset_schema_is_rejected_whole() {
    let emulator = Emulator::new();

    let rows: [SchemaRow; 4] = [
        SchemaRow {
            case: "a predefined example missing the scenario it belongs to",
            name: "predefined_schema",
            schema_type: SCHEMA_TYPES[0],
            index: 1,
            offending: json!({"input": "a turn with no scenario"}),
        },
        SchemaRow {
            case: "a simulated example that is not an object",
            name: "simulated_schema",
            schema_type: SCHEMA_TYPES[1],
            index: 0,
            offending: json!("not an example at all"),
        },
        SchemaRow {
            case: "a third-party example missing its input",
            name: "third_party_schema",
            schema_type: SCHEMA_TYPES[2],
            index: 2,
            offending: json!({"scenario_id": "scenario-x"}),
        },
        SchemaRow {
            case: "a predefined example that is an empty object",
            name: "empty_schema",
            schema_type: SCHEMA_TYPES[0],
            index: 1,
            offending: json!({}),
        },
    ];

    let mut datasets: Vec<(&SchemaRow, String)> = Vec::new();
    for row in &rows {
        let dataset_id = emulator
            .settled_dataset(row.case, row.name, row.schema_type, 2)
            .await;
        datasets.push((row, dataset_id));
    }

    for (row, dataset_id) in &datasets {
        let case = row.case;
        // A batch of three, with the offending example at the row's position.
        let mut examples: Vec<Value> = (0..3).map(example).collect();
        examples[row.index] = row.offending.clone();
        let answer = emulator
            .add_examples(
                dataset_id,
                &json!({"source": {"inlineExamples": {"examples": examples}}}),
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 from a batch failing the schema, got {} and \
             body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {} and body {}",
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("message"),
            format!(
                "Example at index {} failed schema validation: Required field '{}' is missing \
                 from example content",
                row.index,
                required_field(row.schema_type)
            ),
            "case {case:?}: the refusal names the offending example and the field it omits, \
             got {:?}",
            answer.text("message")
        );

        // All-or-nothing: the examples that would have passed are not written either, and
        // the dataset is left in the status it was in.
        let read = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            read.number("exampleCount"),
            2,
            "case {case:?}: a rejected batch writes nothing, got {}",
            read.body["exampleCount"]
        );
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: a rejected batch starts no mutation, got {}",
            read.text("status")
        );
    }
}

/// A `datasetId` naming no dataset is the modelled not-found, in the dataset's own
/// wording, whatever the batch holds.
#[tokio::test]
async fn an_add_naming_no_dataset_is_the_modelled_not_found() {
    let emulator = Emulator::new();

    // Every id is inside `DatasetId`'s pattern `^[a-zA-Z0-9_-]{1,110}$`, so the request
    // reaches the handler: the id names no dataset rather than being malformed.
    let longest = "d".repeat(110);
    let ids = ["missing-dataset", "a", longest.as_str()];
    for dataset_id in ids {
        let case = format!("datasetId {dataset_id:?}");
        let answer = emulator
            .add_examples(dataset_id, &json!({"source": inline_source(1)}))
            .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404 from an add to no dataset, got {} and body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {} and \
             body {}",
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("message"),
            "The specified dataset does not exist. Please verify the datasetId and try again.",
            "case {case:?}: the refusal names the member to verify, got {:?}",
            answer.text("message")
        );
    }
}

/// A replayed `clientToken` adds the batch ONCE: the second request answers the ids the
/// first minted and the working copy grows once, while a request carrying a different
/// token adds its own batch.
#[tokio::test]
async fn a_replayed_client_token_adds_the_batch_once() {
    let emulator = Emulator::new();
    let case = "a replayed clientToken";
    let dataset_id = emulator
        .settled_dataset(case, "replayed_set", SCHEMA_TYPES[1], 1)
        .await;

    let token = client_token("ReplayedAddToken");
    let body = json!({
        "clientToken": token,
        "source": inline_source(3),
    });

    let first = emulator.add_examples(&dataset_id, &body).await;
    assert_eq!(
        first.status, 202,
        "case {case:?}: expected HTTP 202 from the first add, got {} and body {}",
        first.status, first.body
    );
    let minted = first.example_ids();
    assert_minted_ids(case, &minted, 3, &BTreeSet::new());

    let replay = emulator.add_examples(&dataset_id, &body).await;
    assert_eq!(
        replay.status, 202,
        "case {case:?}: a replayed token is served, not refused, got {} and body {}",
        replay.status, replay.body
    );
    assert_eq!(
        replay.example_ids(),
        minted,
        "case {case:?}: the replay answers the ids the first request minted, got {:?}",
        replay.example_ids()
    );
    assert_eq!(
        replay.number("addedCount"),
        3,
        "case {case:?}: the replay reports the batch it added once, got {}",
        replay.body["addedCount"]
    );

    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        4,
        "case {case:?}: the replayed batch was appended once, got {}",
        read.body["exampleCount"]
    );

    // A different token is a different request, so it appends its own batch once the
    // clock has settled the dataset.
    emulator.poll_until_settled(case, &dataset_id).await;
    let other = emulator
        .add_examples(
            &dataset_id,
            &json!({
                "clientToken": client_token("AnotherAddToken"),
                "source": inline_source(2),
            }),
        )
        .await;
    assert_eq!(
        other.status, 202,
        "case {case:?}: expected HTTP 202 for a fresh token, got {} and body {}",
        other.status, other.body
    );
    let already_held: BTreeSet<String> = minted.into_iter().collect();
    assert_minted_ids(case, &other.example_ids(), 2, &already_held);
    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        6,
        "case {case:?}: the fresh token's batch was appended, got {}",
        read.body["exampleCount"]
    );

    // The SAME token against a different dataset is a different request too: a replay
    // answers only for the dataset the token's own batch went to, so this one appends to
    // the dataset it names rather than reporting the other dataset's ids.
    let case = "a replayed token against another dataset";
    let elsewhere = emulator
        .settled_dataset(case, "other_replayed_set", SCHEMA_TYPES[1], 1)
        .await;
    let crossed = emulator.add_examples(&elsewhere, &body).await;
    assert_eq!(
        crossed.status, 202,
        "case {case:?}: expected HTTP 202, got {} and body {}",
        crossed.status, crossed.body
    );
    assert_eq!(
        crossed.text("datasetId"),
        elsewhere,
        "case {case:?}: the add reports the dataset the request named, got {}",
        crossed.text("datasetId")
    );
    assert_minted_ids(case, &crossed.example_ids(), 3, &already_held);
    let read = emulator.get_dataset(&elsewhere).await;
    assert_eq!(
        read.number("exampleCount"),
        4,
        "case {case:?}: the batch was appended to the dataset it named, got {}",
        read.body["exampleCount"]
    );
}

/// The bucket the JSONL files live in, inside `S3Uri`'s pattern
/// `^s3://[a-z0-9][a-z0-9.\-]{1,61}[a-z0-9]/.{1,1024}$` (`smithy_query shape=S3Source`).
const BUCKET: &str = "agentcore-dataset-additions";

/// An Amazon S3 source is read out of this emulator's own object storage: the examples the
/// add counts are the ones the file holds, a URI holding no readable object is refused,
/// and a file that resolves but yields no example settles the dataset in `UPDATE_FAILED`
/// with the reason the mutation failed.
#[tokio::test]
async fn a_batch_is_added_from_a_jsonl_file_in_object_storage() {
    let emulator = Emulator::with_object_storage();
    let schema_type = SCHEMA_TYPES[0];
    let case = "an Amazon S3 source";
    let dataset_id = emulator
        .settled_dataset(case, "file_fed_set", schema_type, 1)
        .await;

    // A URI object storage holds nothing at is refused to the caller, naming the URI,
    // and nothing is written for it.
    let missing = "s3://agentcore-dataset-additions/absent/examples.jsonl";
    let refused = emulator
        .add_examples(&dataset_id, &json!({"source": {"s3Source": {"s3Uri": missing}}}))
        .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400 for a URI naming no object, got {} and body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {} and body {}",
        refused.error_code(),
        refused.body
    );
    assert_eq!(
        refused.text("message"),
        format!("The specified S3 URI does not exist or is not accessible: {missing}"),
        "case {case:?}: the refusal names the URI the request carried, got {:?}",
        refused.text("message")
    );

    // A file holding four examples, one per line, with a blank line between two of them.
    let key = "batches/four.jsonl";
    let file = format!(
        "{}\n{}\n\n{}\n{}\n",
        example(10),
        example(11),
        example(12),
        example(13)
    );
    emulator.put_object(BUCKET, key, file.as_bytes()).await;
    let added = emulator
        .add_examples(
            &dataset_id,
            &json!({"source": {"s3Source": {"s3Uri": format!("s3://{BUCKET}/{key}")}}}),
        )
        .await;
    assert_eq!(
        added.status, 202,
        "case {case:?}: expected HTTP 202 from AddDatasetExamples, got {} and body {}",
        added.status, added.body
    );
    assert_eq!(
        added.number("addedCount"),
        4,
        "case {case:?}: expected the four examples the file holds, got {}",
        added.body["addedCount"]
    );
    assert_minted_ids(case, &added.example_ids(), 4, &BTreeSet::new());
    let read = emulator.poll_until_settled(case, &dataset_id).await;
    assert_eq!(
        read.text("status"),
        "ACTIVE",
        "case {case:?}: a dataset the file's examples were written to settles ACTIVE, got {} \
         and body {}",
        read.text("status"),
        read.body
    );
    assert_eq!(
        read.number("exampleCount"),
        5,
        "case {case:?}: the draft holds its own example plus the file's four, got {}",
        read.body["exampleCount"]
    );

    // A file that resolves but holds no example wrote nothing: the mutation itself failed,
    // which the dataset reports once the clock owes it `UPDATE_FAILED`.
    let empty_key = "batches/empty.jsonl";
    emulator.put_object(BUCKET, empty_key, b"\n\n").await;
    let failed = emulator
        .add_examples(
            &dataset_id,
            &json!({"source": {"s3Source": {"s3Uri": format!("s3://{BUCKET}/{empty_key}")}}}),
        )
        .await;
    assert_eq!(
        failed.status, 202,
        "case {case:?}: an asynchronous mutation is accepted, got {} and body {}",
        failed.status, failed.body
    );
    assert_eq!(
        failed.number("addedCount"),
        0,
        "case {case:?}: a file holding no example adds none, got {}",
        failed.body["addedCount"]
    );
    assert!(
        failed.example_ids().is_empty(),
        "case {case:?}: no example was added, so no id was minted, got {:?}",
        failed.example_ids()
    );
    let read = emulator.poll_until_settled(case, &dataset_id).await;
    assert_eq!(
        read.text("status"),
        "UPDATE_FAILED",
        "case {case:?}: a mutation that wrote nothing settles UPDATE_FAILED, got {} and \
         body {}",
        read.text("status"),
        read.body
    );
    assert!(
        !read.text("failureReason").is_empty(),
        "case {case:?}: a failed mutation reports why, got {}",
        read.body["failureReason"]
    );
    assert_eq!(
        read.number("exampleCount"),
        5,
        "case {case:?}: a failed mutation left the draft as it was, got {}",
        read.body["exampleCount"]
    );
}
