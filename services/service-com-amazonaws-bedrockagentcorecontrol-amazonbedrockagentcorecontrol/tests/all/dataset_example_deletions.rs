// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteDatasetExamples`.
//!
//! The rule under test: a delete removes from the dataset's DRAFT working copy exactly
//! the examples the `ExampleId`s it carries address, reports how many left it, and leaves
//! the dataset `UPDATING` until the clock settles it — resolving every id in the batch
//! before anything is removed, so one id the draft holds no example under refuses the
//! whole call and writes nothing, refusing a write while the dataset is in a status that
//! blocks writes, and refusing a `datasetId` naming no dataset.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised. The store is the one shared
//! engine, so what one request writes is what the next request reads.
//!
//! The ids a delete names are the ones `AddDatasetExamples` minted for the examples it
//! appended, and what the draft holds afterwards is read back through `GetDataset`, so
//! every claim is checked against the service's own state rather than against this file.

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

/// A `clientToken` inside the modelled `ClientToken` constraints (len 33..256,
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`, `smithy_query operation=DeleteDatasetExamples`).
fn client_token(seed: &str) -> String {
    let token = format!("{seed}{}", "0".repeat(40));
    token[..40].to_string()
}

/// This emulator: the control plane over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // `default()` registers no authenticator, so the suite exercises routing and the
        // handlers rather than this process's SigV4 keys.
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        Self { router }
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

    /// `AddDatasetExamples`: `POST /datasets/{datasetId}/examples/add`.
    async fn add_examples(&self, dataset_id: &str, body: &Value) -> Answer {
        self.call(
            "POST",
            &format!("/datasets/{dataset_id}/examples/add"),
            Some(body),
        )
        .await
    }

    /// `DeleteDatasetExamples`: `POST /datasets/{datasetId}/examples/delete`
    /// (`smithy_query operation=DeleteDatasetExamples`).
    async fn delete_examples(&self, dataset_id: &str, body: &Value) -> Answer {
        self.call(
            "POST",
            &format!("/datasets/{dataset_id}/examples/delete"),
            Some(body),
        )
        .await
    }

    /// `ListDatasetExamples`: `GET /datasets/{datasetId}/examples`, with the query
    /// string the caller wants (`smithy_query operation=ListDatasetExamples`).
    async fn list_examples(&self, dataset_id: &str, query: &str) -> Answer {
        let uri = if query.is_empty() {
            format!("/datasets/{dataset_id}/examples")
        } else {
            format!("/datasets/{dataset_id}/examples?{query}")
        };
        self.call("GET", &uri, None).await
    }

    /// `CreateDatasetVersion`: `POST /datasets/{datasetId}/versions`.
    async fn create_version(&self, dataset_id: &str) -> Answer {
        self.call("POST", &format!("/datasets/{dataset_id}/versions"), None)
            .await
    }

    /// Every `exampleId` the listing reports for a version of this dataset, in page
    /// order, following `nextToken` to the end.
    async fn listed_ids(&self, case: &str, dataset_id: &str, version: &str) -> Vec<String> {
        let mut ids = Vec::new();
        let mut query = format!("datasetVersion={version}&maxResults=2");
        loop {
            let page = self.list_examples(dataset_id, &query).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected HTTP 200 from ListDatasetExamples, got {} and body {}",
                page.status, page.body
            );
            assert_eq!(
                page.text("datasetVersion"),
                version,
                "case {case:?}: the listing reports the version it resolved to, got {}",
                page.text("datasetVersion")
            );
            ids.extend(page.listed_example_ids(case));
            match page.body.get("nextToken").and_then(Value::as_str) {
                // A later page is PINNED to the version the token carries, so the
                // request that follows it names no version of its own.
                Some(token) => query = format!("maxResults=2&nextToken={token}"),
                None => return ids,
            }
        }
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

    /// Append `count` examples to the dataset's draft and answer the ids the add minted.
    async fn added_ids(&self, case: &str, dataset_id: &str, count: usize) -> Vec<String> {
        let added = self
            .add_examples(dataset_id, &json!({"source": inline_source(count)}))
            .await;
        assert_eq!(
            added.status, 202,
            "case {case:?}: expected HTTP 202 from AddDatasetExamples, got {} and body {}",
            added.status, added.body
        );
        let ids = added.example_ids();
        assert_eq!(
            ids.len(),
            count,
            "case {case:?}: expected one exampleId per added example ({count}), got {ids:?}"
        );
        ids
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

    /// The `exampleId` of every example one page of a listing reported. Each element
    /// is "a JSON object containing at least an exampleId field plus the
    /// schema-specific content fields" (`smithy_query
    /// shape=ListDatasetExamplesResponse member=examples`), so every element must
    /// carry both.
    fn listed_example_ids(&self, case: &str) -> Vec<String> {
        self.body
            .get("examples")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a listing reports its examples as a list, got {}",
                    self.body
                )
            })
            .iter()
            .map(|listed| {
                let id = listed
                    .get("exampleId")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a listed example carries its exampleId, got {listed}")
                    });
                assert!(
                    listed.get("scenario_id").is_some() || listed.get("input").is_some(),
                    "case {case:?}: a listed example carries its content fields, got {listed}"
                );
                id.to_string()
            })
            .collect()
    }
}

/// One example carrying the fields both AgentCore evaluation schemas and the third-party
/// schema require (`crate::state::required_example_fields`), so the same batch satisfies
/// every `DatasetSchemaType`.
fn example(index: usize) -> Value {
    json!({
        "scenario_id": format!("scenario-{index}"),
        "input": format!("turn {index}"),
        "expected": {"score": index},
    })
}

/// A `source` carrying `count` inline examples, each satisfying every schema type.
fn inline_source(count: usize) -> Value {
    let examples: Vec<Value> = (0..count).map(example).collect();
    json!({"inlineExamples": {"examples": examples}})
}

/// Assert one `Timestamp` member's wire form: an epoch-seconds NUMBER naming a real
/// instant. `updatedAt` targets the prelude `Timestamp` with no `@timestampFormat` of its
/// own (`smithy_query operation=DeleteDatasetExamples`), which `restJson1` renders as
/// epoch seconds.
fn assert_epoch_seconds(case: &str, member: &str, value: &Value) {
    let seconds = value.as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be an epoch-seconds number, got {value:?}")
    });
    assert!(
        seconds > 0.0,
        "case {case:?}: {member} must name a real instant, got {seconds}"
    );
}

/// One delete of examples by id: the dataset it shrinks, and which of the ids an add
/// minted the request names.
struct DeleteRow {
    case: &'static str,
    name: &'static str,
    schema_type: &'static str,
    /// How many examples the create's source yielded.
    initial: usize,
    /// How many examples an add appended, whose ids the delete names.
    added: usize,
    /// Positions in those added ids the delete's `exampleIds` carries, in order.
    positions: &'static [usize],
    /// How many examples the delete takes out of the draft: one per DISTINCT position.
    deleted: usize,
}

/// The ids at `positions` of `minted`, in the order the row lists them.
fn picked(minted: &[String], positions: &[usize]) -> Vec<String> {
    positions
        .iter()
        .map(|position| minted[*position].clone())
        .collect()
}

/// The examples the ids name leave the DRAFT working copy, the delete reports how many
/// went, and the dataset reads back holding the rest.
///
/// The rows differ along what the rule generalises over: the dataset's schema type (every
/// `DatasetSchemaType` value), how many examples the dataset holds, how many ids the batch
/// carries, WHICH examples they address (the first, the last, a scattered few, all of
/// them), and a batch that names one id twice.
#[tokio::test]
async fn examples_named_by_id_leave_the_draft_and_the_dataset_reads_back_holding_the_rest() {
    let emulator = Emulator::new();

    let rows: [DeleteRow; 5] = [
        DeleteRow {
            case: "the first of the added examples",
            name: "first_dropped",
            schema_type: SCHEMA_TYPES[0],
            initial: 1,
            added: 3,
            positions: &[0],
            deleted: 1,
        },
        DeleteRow {
            case: "the last of the added examples",
            name: "last_dropped",
            schema_type: SCHEMA_TYPES[1],
            initial: 2,
            added: 3,
            positions: &[2],
            deleted: 1,
        },
        DeleteRow {
            case: "a scattered few of the added examples",
            name: "several_dropped",
            schema_type: SCHEMA_TYPES[2],
            initial: 2,
            added: 4,
            positions: &[3, 0, 2],
            deleted: 3,
        },
        DeleteRow {
            case: "a batch naming one example twice",
            name: "repeated_dropped",
            schema_type: SCHEMA_TYPES[0],
            initial: 1,
            added: 2,
            positions: &[1, 1, 0],
            deleted: 2,
        },
        DeleteRow {
            case: "every example the add appended",
            name: "all_dropped",
            schema_type: SCHEMA_TYPES[1],
            initial: 1,
            added: 2,
            positions: &[0, 1],
            deleted: 2,
        },
    ];

    // The datasets the table needs, created and grown in phases, so a clock-owed
    // transition is waited out ONCE per phase rather than once per row.
    let mut created: Vec<(&DeleteRow, String)> = Vec::new();
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
        emulator.poll_until_settled(row.case, dataset_id).await;
    }

    let mut grown: Vec<(&DeleteRow, String, Vec<String>)> = Vec::new();
    for (row, dataset_id) in &created {
        let minted = emulator.added_ids(row.case, dataset_id, row.added).await;
        grown.push((*row, dataset_id.clone(), minted));
    }
    for (row, dataset_id, _) in &grown {
        emulator.poll_until_settled(row.case, dataset_id).await;
    }

    for (row, dataset_id, minted) in &grown {
        let case = row.case;
        let ids = picked(minted, row.positions);
        let answer = emulator
            .delete_examples(dataset_id, &json!({"exampleIds": ids}))
            .await;
        assert_eq!(
            answer.status, 202,
            "case {case:?}: expected HTTP 202 from DeleteDatasetExamples, got {} and body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.text("datasetId"),
            *dataset_id,
            "case {case:?}: the delete reports the dataset it shrank, got {}",
            answer.text("datasetId")
        );
        assert_eq!(
            answer.text("datasetArn"),
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:dataset/{dataset_id}"),
            "case {case:?}: expected the modelled DatasetArn for {dataset_id}, got {}",
            answer.text("datasetArn")
        );
        assert_eq!(
            answer.number("deletedCount"),
            i64::try_from(row.deleted).expect("the deleted count fits a Long"),
            "case {case:?}: expected {} examples deleted, got {}",
            row.deleted,
            answer.body["deletedCount"]
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

        // The examples are gone from the working copy as soon as the call answers: only
        // the STATUS is owed to the clock.
        let remaining = row.initial + row.added - row.deleted;
        let read = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            read.number("exampleCount"),
            i64::try_from(remaining).expect("the example count fits a Long"),
            "case {case:?}: expected {remaining} examples left in the draft, got {}",
            read.body["exampleCount"]
        );
        assert_eq!(
            read.text("draftStatus"),
            "MODIFIED",
            "case {case:?}: a draft the delete changed reflects no published version, got {}",
            read.text("draftStatus")
        );
    }

    for (row, dataset_id, minted) in &grown {
        let case = row.case;
        let read = emulator.poll_until_settled(case, dataset_id).await;
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: a dataset the delete wrote settles ACTIVE, got {} and body {}",
            read.text("status"),
            read.body
        );
        let remaining = row.initial + row.added - row.deleted;
        assert_eq!(
            read.number("exampleCount"),
            i64::try_from(remaining).expect("the example count fits a Long"),
            "case {case:?}: the settled dataset still holds the examples the delete left, \
             got {}",
            read.body["exampleCount"]
        );
        assert!(
            read.body["failureReason"].is_null(),
            "case {case:?}: a delete that removed its examples reports no failureReason, \
             got {}",
            read.body["failureReason"]
        );

        // The round trip: the examples the delete named are gone from the working copy
        // and every other example of it is still listed, under the id it entered with.
        // The listing is paged two at a time, so the walk follows the tokens it issues.
        let listed = emulator.listed_ids(case, dataset_id, "DRAFT").await;
        assert_eq!(
            listed.len(),
            remaining,
            "case {case:?}: expected {remaining} examples listed in the draft, got {listed:?}"
        );
        for (position, id) in minted.iter().enumerate() {
            let deleted = row.positions.contains(&position);
            assert_eq!(
                !listed.contains(id),
                deleted,
                "case {case:?}: example {id} was {}, so the listing {} report it; got {listed:?}",
                if deleted { "deleted" } else { "kept" },
                if deleted { "must not" } else { "must" }
            );
        }

        // The ids are gone for good: a repeat naming them finds no such example in the
        // DRAFT, and the working copy is left as the first delete left it.
        let ids = picked(minted, row.positions);
        let again = emulator
            .delete_examples(dataset_id, &json!({"exampleIds": ids.clone()}))
            .await;
        assert_eq!(
            again.status, 404,
            "case {case:?}: expected HTTP 404 from a repeat naming deleted examples, got {} \
             and body {}",
            again.status, again.body
        );
        assert_eq!(
            again.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {} and \
             body {}",
            again.error_code(),
            again.body
        );
        assert_eq!(
            again.text("message"),
            format!(
                "The specified example does not exist in DRAFT: {}. Please verify the \
                 exampleIds and try again.",
                ids[0]
            ),
            "case {case:?}: the refusal names the id the draft holds no example under, got {:?}",
            again.text("message")
        );
        let read = emulator.get_dataset(dataset_id).await;
        assert_eq!(
            read.number("exampleCount"),
            i64::try_from(remaining).expect("the example count fits a Long"),
            "case {case:?}: the refused repeat removed nothing, got {}",
            read.body["exampleCount"]
        );
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: a refused delete starts no mutation, got {}",
            read.text("status")
        );
    }
}

/// One batch naming an example the DRAFT does not hold: where in the batch the offending
/// id sits, and what makes it offend.
struct AbsentRow {
    case: &'static str,
    /// Position in the batch the offending id takes, among the ids the add minted.
    at: usize,
    /// How the offending id is reached: an id no dataset ever minted, or one minted for
    /// a DIFFERENT dataset's example.
    from_other_dataset: bool,
    /// The id itself, when it is not another dataset's.
    literal: &'static str,
}

/// A batch naming one example the DRAFT holds nothing under is rejected WHOLE: nothing of
/// it is removed and the dataset stays in the status it was in.
///
/// The rows differ along what the rule generalises over: where the offending id sits in
/// the batch (first, in the middle, last) and where it comes from — an id this service
/// never minted, in both the shortest and the longest form `ExampleId` admits, and an id
/// that IS a real example's, but of another dataset.
#[tokio::test]
async fn a_batch_naming_an_absent_example_is_rejected_whole() {
    let emulator = Emulator::new();
    let case = "the dataset under test";
    let dataset_id = emulator
        .settled_dataset(case, "absent_ids", SCHEMA_TYPES[0], 2)
        .await;
    let minted = emulator.added_ids(case, &dataset_id, 3).await;
    emulator.poll_until_settled(case, &dataset_id).await;

    // Another dataset's example, so one of the rows can name a real id that this dataset
    // does not hold.
    let elsewhere = emulator
        .settled_dataset(case, "other_examples", SCHEMA_TYPES[0], 1)
        .await;
    let other_ids = emulator.added_ids(case, &elsewhere, 1).await;
    emulator.poll_until_settled(case, &elsewhere).await;

    // `ExampleId` is `^[a-zA-Z0-9_.:-]+$` with `length 0..256` (`smithy_query
    // shape=ExampleIdList`), so every literal below reaches the handler: it names no
    // example rather than being malformed.
    let longest: String = "e".repeat(256);
    let rows: [AbsentRow; 4] = [
        AbsentRow {
            case: "an unminted id first in the batch",
            at: 0,
            from_other_dataset: false,
            literal: "no.such:example-1",
        },
        AbsentRow {
            case: "an unminted id in the middle of the batch",
            at: 1,
            from_other_dataset: false,
            literal: "e",
        },
        AbsentRow {
            case: "an unminted id last in the batch",
            at: 3,
            from_other_dataset: false,
            literal: "",
        },
        AbsentRow {
            case: "another dataset's example id",
            at: 2,
            from_other_dataset: true,
            literal: "",
        },
    ];

    for row in &rows {
        let case = row.case;
        let offending = if row.from_other_dataset {
            other_ids[0].clone()
        } else if row.literal.is_empty() {
            longest.clone()
        } else {
            row.literal.to_string()
        };
        let mut ids = minted.clone();
        ids.insert(row.at, offending.clone());

        let answer = emulator
            .delete_examples(&dataset_id, &json!({"exampleIds": ids}))
            .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404 from a batch naming an absent example, got {} \
             and body {}",
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
            format!(
                "The specified example does not exist in DRAFT: {offending}. Please verify \
                 the exampleIds and try again."
            ),
            "case {case:?}: the refusal names the offending id, got {:?}",
            answer.text("message")
        );

        // All-or-nothing: the ids that WOULD have resolved are not removed either, and
        // the dataset is left in the status it was in.
        let read = emulator.get_dataset(&dataset_id).await;
        assert_eq!(
            read.number("exampleCount"),
            5,
            "case {case:?}: a rejected batch removes nothing, got {}",
            read.body["exampleCount"]
        );
        assert_eq!(
            read.text("status"),
            "ACTIVE",
            "case {case:?}: a rejected batch starts no mutation, got {}",
            read.text("status")
        );
    }

    // The other dataset kept its own example throughout.
    let read = emulator.get_dataset(&elsewhere).await;
    assert_eq!(
        read.number("exampleCount"),
        2,
        "case \"another dataset\": a refused delete touched no other dataset, got {}",
        read.body["exampleCount"]
    );
}

/// A dataset in a status that blocks writes refuses the delete with the modelled
/// conflict, and serves it again once the clock has settled the dataset.
///
/// The rows are the transient statuses the model says block every write: the `CREATING`
/// of an ingestion still in flight, and the `UPDATING` of an example mutation still in
/// flight — which is what a second delete racing the first one meets.
#[tokio::test]
async fn a_delete_is_refused_while_the_dataset_blocks_writes() {
    let emulator = Emulator::new();

    let created = emulator
        .create_dataset(&json!({
            "datasetName": "busy_deletes",
            "schemaType": SCHEMA_TYPES[0],
            "source": inline_source(2),
        }))
        .await;
    let dataset_id = created.text("datasetId");

    // A dataset whose own ingestion is still in flight is `CREATING`, in which "all
    // writes are blocked". The id is resolved only once the status allows the write, so
    // an id this dataset does not hold is still the conflict.
    let during_create = emulator
        .delete_examples(&dataset_id, &json!({"exampleIds": ["any.id"]}))
        .await;
    assert_eq!(
        during_create.status, 409,
        "case \"CREATING\": expected HTTP 409 from a delete on a creating dataset, got {} and \
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
    let minted = emulator.added_ids("UPDATING", &dataset_id, 3).await;
    emulator.poll_until_settled("UPDATING", &dataset_id).await;

    let first = emulator
        .delete_examples(&dataset_id, &json!({"exampleIds": [minted[0].clone()]}))
        .await;
    assert_eq!(
        first.status, 202,
        "case \"UPDATING\": expected HTTP 202 from the first delete, got {} and body {}",
        first.status, first.body
    );

    // The mutation that delete started is in flight, so the dataset is `UPDATING` and the
    // next delete conflicts with it rather than racing it — whether it names the example
    // the first one took or another one still in the draft.
    for (case, id) in [
        ("a second delete of the same example", minted[0].clone()),
        ("a concurrent delete of another example", minted[1].clone()),
    ] {
        let during_update = emulator
            .delete_examples(&dataset_id, &json!({"exampleIds": [id]}))
            .await;
        assert_eq!(
            during_update.status, 409,
            "case {case:?}: expected HTTP 409 from a concurrent delete, got {} and body {}",
            during_update.status, during_update.body
        );
        assert_eq!(
            during_update.error_code(),
            "ConflictException",
            "case {case:?}: expected the modelled ConflictException, got {} and body {}",
            during_update.error_code(),
            during_update.body
        );
        assert_eq!(
            during_update.text("message"),
            "Dataset is not in a state that allows this operation: UPDATING",
            "case {case:?}: the refusal names the state the dataset is in, got {:?}",
            during_update.text("message")
        );
    }

    let refused = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        refused.number("exampleCount"),
        4,
        "case \"UPDATING\": the refused deletes removed nothing beyond the first, got {}",
        refused.body["exampleCount"]
    );

    // The conflict is transient: once the clock settles the dataset, the same delete is
    // served.
    emulator.poll_until_settled("UPDATING", &dataset_id).await;
    let after_settle = emulator
        .delete_examples(&dataset_id, &json!({"exampleIds": [minted[1].clone()]}))
        .await;
    assert_eq!(
        after_settle.status, 202,
        "case \"settled\": expected HTTP 202 from a delete on a settled dataset, got {} and \
         body {}",
        after_settle.status, after_settle.body
    );
    assert_eq!(
        after_settle.number("deletedCount"),
        1,
        "case \"settled\": expected the one named example deleted, got {}",
        after_settle.body["deletedCount"]
    );
    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        3,
        "case \"settled\": the delete removed its example, got {}",
        read.body["exampleCount"]
    );
}

/// "Deletes specific examples by ID from DRAFT": a delete takes the examples out of the
/// working copy ONLY, so a version published before it still holds them — and a delete
/// naming an example that is in a published version but no longer in the DRAFT is
/// refused, because the ids are resolved against the working copy.
#[tokio::test]
async fn a_delete_takes_examples_out_of_the_draft_only() {
    let emulator = Emulator::new();
    let case = "a published version";
    let dataset_id = emulator
        .settled_dataset(case, "published_set", SCHEMA_TYPES[2], 1)
        .await;
    let minted = emulator.added_ids(case, &dataset_id, 3).await;
    emulator.poll_until_settled(case, &dataset_id).await;

    let published = emulator.create_version(&dataset_id).await;
    assert_eq!(
        published.status, 202,
        "case {case:?}: expected HTTP 202 from CreateDatasetVersion, got {} and body {}",
        published.status, published.body
    );
    let version = published.text("datasetVersion");
    emulator.poll_until_settled(case, &dataset_id).await;

    let deleted = emulator
        .delete_examples(&dataset_id, &json!({"exampleIds": [minted[0].clone()]}))
        .await;
    assert_eq!(
        deleted.status, 202,
        "case {case:?}: expected HTTP 202 from DeleteDatasetExamples, got {} and body {}",
        deleted.status, deleted.body
    );
    emulator.poll_until_settled(case, &dataset_id).await;

    let draft = emulator.listed_ids(case, &dataset_id, "DRAFT").await;
    assert!(
        !draft.contains(&minted[0]),
        "case {case:?}: the deleted example left the draft, got {draft:?}"
    );
    assert_eq!(
        draft.len(),
        3,
        "case {case:?}: the draft holds the three examples the delete left, got {draft:?}"
    );

    // The version was published before the delete, and "the DRAFT is preserved and
    // remains editable after publishing": the snapshot keeps the example the draft lost.
    let snapshot = emulator.listed_ids(case, &dataset_id, &version).await;
    assert!(
        snapshot.contains(&minted[0]),
        "case {case:?}: version {version} keeps the example the draft lost, got {snapshot:?}"
    );
    assert_eq!(
        snapshot.len(),
        4,
        "case {case:?}: version {version} holds every example the draft had when it was \
         published, got {snapshot:?}"
    );
    let read = emulator
        .call(
            "GET",
            &format!("/datasets/{dataset_id}?datasetVersion={version}"),
            None,
        )
        .await;
    assert_eq!(
        read.number("exampleCount"),
        4,
        "case {case:?}: the published version's count is unchanged by the delete, got {}",
        read.body["exampleCount"]
    );

    // An id the published version holds but the DRAFT no longer does is resolved against
    // the working copy, so naming it again is the modelled not-found.
    let again = emulator
        .delete_examples(&dataset_id, &json!({"exampleIds": [minted[0].clone()]}))
        .await;
    assert_eq!(
        again.status, 404,
        "case {case:?}: expected HTTP 404 for an example only a published version holds, \
         got {} and body {}",
        again.status, again.body
    );
    assert_eq!(
        again.text("message"),
        format!(
            "The specified example does not exist in DRAFT: {}. Please verify the exampleIds \
             and try again.",
            minted[0]
        ),
        "case {case:?}: the refusal names the id the draft holds no example under, got {:?}",
        again.text("message")
    );
}

/// A `datasetId` naming no dataset is the modelled not-found, in the dataset's own
/// wording, whatever the batch names.
#[tokio::test]
async fn a_delete_naming_no_dataset_is_the_modelled_not_found() {
    let emulator = Emulator::new();

    // Every id is inside `DatasetId`'s pattern `^[a-zA-Z0-9_-]{1,110}$`, so the request
    // reaches the handler: the id names no dataset rather than being malformed.
    let longest = "d".repeat(110);
    for dataset_id in ["missing-dataset", "a", longest.as_str()] {
        let case = format!("datasetId {dataset_id:?}");
        let answer = emulator
            .delete_examples(dataset_id, &json!({"exampleIds": ["some.example"]}))
            .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404 from a delete on no dataset, got {} and body {}",
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

/// A replayed `clientToken` deletes the batch ONCE: the second request reports the
/// examples the first one removed instead of refusing ids the draft no longer holds, and
/// the working copy shrinks once.
#[tokio::test]
async fn a_replayed_client_token_deletes_the_batch_once() {
    let emulator = Emulator::new();
    let case = "a replayed clientToken";
    let dataset_id = emulator
        .settled_dataset(case, "replayed_deletes", SCHEMA_TYPES[1], 1)
        .await;
    let minted = emulator.added_ids(case, &dataset_id, 4).await;
    emulator.poll_until_settled(case, &dataset_id).await;

    let token = client_token("ReplayedDeleteToken");
    let body = json!({
        "clientToken": token,
        "exampleIds": [minted[0].clone(), minted[1].clone()],
    });

    let first = emulator.delete_examples(&dataset_id, &body).await;
    assert_eq!(
        first.status, 202,
        "case {case:?}: expected HTTP 202 from the first delete, got {} and body {}",
        first.status, first.body
    );
    assert_eq!(
        first.number("deletedCount"),
        2,
        "case {case:?}: expected the two named examples deleted, got {}",
        first.body["deletedCount"]
    );

    let replay = emulator.delete_examples(&dataset_id, &body).await;
    assert_eq!(
        replay.status, 202,
        "case {case:?}: a replayed token is served, not refused, got {} and body {}",
        replay.status, replay.body
    );
    assert_eq!(
        replay.number("deletedCount"),
        2,
        "case {case:?}: the replay reports the batch it deleted once, got {}",
        replay.body["deletedCount"]
    );

    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        3,
        "case {case:?}: the replayed batch was removed once, got {}",
        read.body["exampleCount"]
    );

    // A different token is a different request, so it removes its own examples once the
    // clock has settled the dataset.
    emulator.poll_until_settled(case, &dataset_id).await;
    let other = emulator
        .delete_examples(
            &dataset_id,
            &json!({
                "clientToken": client_token("AnotherDeleteToken"),
                "exampleIds": [minted[2].clone()],
            }),
        )
        .await;
    assert_eq!(
        other.status, 202,
        "case {case:?}: expected HTTP 202 for a fresh token, got {} and body {}",
        other.status, other.body
    );
    assert_eq!(
        other.number("deletedCount"),
        1,
        "case {case:?}: the fresh token's batch removed its one example, got {}",
        other.body["deletedCount"]
    );
    let read = emulator.get_dataset(&dataset_id).await;
    assert_eq!(
        read.number("exampleCount"),
        2,
        "case {case:?}: the fresh token's example was removed, got {}",
        read.body["exampleCount"]
    );

    // The SAME token against another dataset is a different request too: a replay answers
    // only for the dataset the token's own batch was deleted from, so this one is served
    // fresh — and the ids it carries belong to no example of the dataset it names.
    let case = "a replayed token against another dataset";
    let elsewhere = emulator
        .settled_dataset(case, "other_replayed_deletes", SCHEMA_TYPES[1], 2)
        .await;
    let crossed = emulator.delete_examples(&elsewhere, &body).await;
    assert_eq!(
        crossed.status, 404,
        "case {case:?}: expected HTTP 404, got {} and body {}",
        crossed.status, crossed.body
    );
    assert_eq!(
        crossed.text("message"),
        format!(
            "The specified example does not exist in DRAFT: {}. Please verify the exampleIds \
             and try again.",
            minted[0]
        ),
        "case {case:?}: the refusal names the id the named dataset holds nothing under, got {:?}",
        crossed.text("message")
    );
    let read = emulator.get_dataset(&elsewhere).await;
    assert_eq!(
        read.number("exampleCount"),
        2,
        "case {case:?}: the other dataset kept its examples, got {}",
        read.body["exampleCount"]
    );
}
