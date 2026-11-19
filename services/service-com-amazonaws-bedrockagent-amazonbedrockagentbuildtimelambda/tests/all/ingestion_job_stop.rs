// SPDX-License-Identifier: CC0-1.0

//! `StopIngestionJob`, end to end over HTTP: a job whose run has not finished is
//! halted — it reports the modelled `STOPPING` status, the clock takes it to
//! `STOPPED`, and it never reaches `COMPLETE` — while a job that has already
//! reached a terminal status has no run to stop and is refused.
//!
//! The halt is also confined and dated: it keeps the instant the halted job
//! started and stamps the halt on `updatedAt`, as the modelled members document,
//! and it reaches no job other than the one its knowledge base, data source and
//! job triple names.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The `IngestionJobStatus` values a job never leaves: the run is over, whether it
/// finished or was halted.
///
/// Source: the `IngestionJobStatus` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="IngestionJobStatus")`), whose other members
/// name the statuses a run passes through.
const TERMINAL_STATUSES: [&str; 2] = ["COMPLETE", "STOPPED"];

/// The `IngestionJobStatus` values only a halt reaches: a job reports either one
/// because `StopIngestionJob` was called for THAT job, never because another job
/// was stopped.
///
/// Source: the same `IngestionJobStatus` enum of this service's Smithy model,
/// whose remaining members (`STARTING`, `IN_PROGRESS`, `COMPLETE`, `FAILED`) are
/// the chain of a run nobody halted.
const HALT_STATUSES: [&str; 2] = ["STOPPING", "STOPPED"];

/// How long a poll for a status the CLOCK owes keeps looking before it reports the
/// transition never came. Deliberately far longer than any span this service owes a
/// job — more than two orders of magnitude — so a host running the whole workspace's
/// tests at once does not fail a test about a status rather than about a duration;
/// the poll stops as soon as the status holds.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How many times a terminal status is read back, to show the clock moves the job
/// no further once its run is over.
const SETTLED_READS: usize = 3;

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// A member of the response's `ingestionJob`.
    fn job(&self, member: &str) -> &Value {
        &self.body["ingestionJob"][member]
    }

    fn job_string(&self, member: &str) -> String {
        self.job(member)
            .as_str()
            .unwrap_or_else(|| panic!("ingestionJob.{member} is not a string in {:?}", self.body))
            .to_string()
    }

    /// A timestamp member of the response's `ingestionJob`, as the instant the
    /// modelled `date-time` wire form names.
    fn job_instant(&self, member: &str) -> f64 {
        crate::update_round_trip::date_time_wire_instant("the stored job", member, self.job(member))
    }

    /// The error's `message`, as the modelled error member carries it.
    fn message(&self) -> String {
        self.body["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no error message in {:?}", self.body))
            .to_string()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // The knowledge bases here bind to an S3 Vectors index, which
        // `CreateKnowledgeBase` resolves in the service that owns it — so the
        // index is created first, as a caller creates it first against AWS.
        crate::vector_store_fixture::seed_index(
            &engine,
            "arn:aws:s3vectors:::bucket/stop-demo-vectors",
            "stop-demo-index",
            crate::vector_store_fixture::EMBEDDING_DIMENSION,
        );
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|e| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// Create a knowledge base and answer its minted `knowledgeBaseId`.
    async fn knowledge_base(&self, name: &str) -> String {
        let request = json!({
            "name": name,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": EMBEDDING_MODEL },
            },
            "storageConfiguration": {
                "type": "S3_VECTORS",
                "s3VectorsConfiguration": {
                    "vectorBucketArn": "arn:aws:s3vectors:::bucket/stop-demo-vectors",
                    "indexName": "stop-demo-index",
                },
            },
        });
        let created = self.call("PUT", "/knowledgebases/", Some(request)).await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.body["knowledgeBase"]["knowledgeBaseId"]
            .as_str()
            .expect("a minted knowledgeBaseId")
            .to_string()
    }

    /// Attach a data source to `knowledge_base_id` and answer its minted
    /// `dataSourceId`.
    async fn data_source(&self, knowledge_base_id: &str, request: Value) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(request),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case setup: expected CreateDataSource -> 200, got {created:?}"
        );
        created.body["dataSource"]["dataSourceId"]
            .as_str()
            .expect("a minted dataSourceId")
            .to_string()
    }

    async fn start_ingestion_job(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) -> Answer {
        self.call(
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs/"
            ),
            Some(request),
        )
        .await
    }

    async fn get_ingestion_job(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        ingestion_job_id: &str,
    ) -> Answer {
        self.call(
            "GET",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/{ingestion_job_id}"
            ),
            None,
        )
        .await
    }

    async fn stop_ingestion_job(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        ingestion_job_id: &str,
    ) -> Answer {
        self.call(
            "POST",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/{ingestion_job_id}/stop"
            ),
            None,
        )
        .await
    }

    /// Read the job until its status is terminal, or until a generous deadline
    /// passes — the CONDITION is waited for, never a fixed duration.
    async fn poll_until_terminal(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        ingestion_job_id: &str,
    ) -> Answer {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let answer = self
                .get_ingestion_job(knowledge_base_id, data_source_id, ingestion_job_id)
                .await;
            let status = answer.job_string("status");
            if TERMINAL_STATUSES.contains(&status.as_str()) {
                return answer;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the job never reached a terminal status, last status {status:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// An S3 data source: its documents are crawled from a bucket.
fn s3_data_source(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceConfiguration": {
            "type": "S3",
            "s3Configuration": { "bucketArn": "arn:aws:s3:::stop-demo-docs" },
        },
    })
}

/// A CUSTOM data source: its documents are pushed in rather than crawled.
fn custom_data_source(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceConfiguration": { "type": "CUSTOM" },
    })
}

/// RULE: `StopIngestionJob` halts a job whose run has not finished — the job
/// reports the modelled `STOPPING` status at once, the clock takes it to `STOPPED`
/// and no further status is owed it, so it never reaches `COMPLETE` — and the data
/// source it ran for serves a new job again.
#[tokio::test]
async fn stopping_a_running_job_halts_it_short_of_complete() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the kind of data
    // source the job ingests (crawled, pushed) and which optional members the
    // start carried (none, a description, an `@idempotencyToken`).
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "a job over a crawled data source, started with its required members only",
            s3_data_source("stop-s3-documents"),
            json!({}),
        ),
        (
            "a job over a pushed data source, started with a description",
            custom_data_source("stop_custom_documents"),
            json!({ "description": "the nightly crawl of the demo bucket" }),
        ),
        (
            "a job over a crawled data source, started with a client token",
            s3_data_source("stop-s3-documents-2"),
            json!({ "clientToken": "StopIngestionTokenForTheSecondSource" }),
        ),
    ];

    for (index, (case, data_source_request, start_request)) in cases.into_iter().enumerate() {
        let knowledge_base_id = emulator.knowledge_base(&format!("stop-kb-{index}")).await;
        let data_source_id = emulator
            .data_source(&knowledge_base_id, data_source_request)
            .await;
        let started = emulator
            .start_ingestion_job(&knowledge_base_id, &data_source_id, start_request)
            .await;
        assert_eq!(
            started.status, 202,
            "case {case:?}: expected StartIngestionJob -> 202, got {started:?}"
        );
        let ingestion_job_id = started.job_string("ingestionJobId");

        let stopped = emulator
            .stop_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            stopped.status, 202,
            "case {case:?}: expected StopIngestionJob -> 202, got {stopped:?}"
        );
        assert_eq!(
            stopped.job_string("status"),
            "STOPPING",
            "case {case:?}: a job this call halted is stopping, got {stopped:?}"
        );
        assert_eq!(
            stopped.job_string("ingestionJobId"),
            ingestion_job_id,
            "case {case:?}: the answer reports the job the URI named, got {:?}",
            stopped.job("ingestionJobId")
        );
        assert_eq!(
            stopped.job_string("knowledgeBaseId"),
            knowledge_base_id,
            "case {case:?}: the answer reports the job's knowledge base, got {:?}",
            stopped.job("knowledgeBaseId")
        );
        assert_eq!(
            stopped.job_string("dataSourceId"),
            data_source_id,
            "case {case:?}: the answer reports the job's data source, got {:?}",
            stopped.job("dataSourceId")
        );
        assert!(
            stopped.job("textReadyAt").is_null(),
            "case {case:?}: no text became queryable on a halted run, got {:?}",
            stopped.job("textReadyAt")
        );

        // The stop is reported by the job's own reader as well, not just by the
        // call that accepted it.
        let read_back = emulator
            .get_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            read_back.status, 200,
            "case {case:?}: expected GetIngestionJob -> 200, got {read_back:?}"
        );
        assert_ne!(
            read_back.job_string("status"),
            "COMPLETE",
            "case {case:?}: a halted run never finishes, got {read_back:?}"
        );

        // The CLOCK finishes the halt: the job settles at STOPPED and stays
        // there, however often it is read.
        let settled = emulator
            .poll_until_terminal(case, &knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            settled.job_string("status"),
            "STOPPED",
            "case {case:?}: the clock owes a halted job only STOPPED, got {settled:?}"
        );
        assert!(
            settled.job("textReadyAt").is_null(),
            "case {case:?}: a halted run extracted no text, got {:?}",
            settled.job("textReadyAt")
        );
        for read in 0..SETTLED_READS {
            let again = emulator
                .get_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
                .await;
            assert_eq!(
                again.body, settled.body,
                "case {case:?}: read {read} of a stopped job must report exactly what the \
                 one before it reported"
            );
        }

        // "You can send a StartIngestionJob request again to ingest the rest of
        // your data": the halted job no longer occupies its data source.
        let restarted = emulator
            .start_ingestion_job(&knowledge_base_id, &data_source_id, json!({}))
            .await;
        assert_eq!(
            restarted.status, 202,
            "case {case:?}: a data source whose job was stopped serves a new job, got \
             {restarted:?}"
        );
        assert_ne!(
            restarted.job_string("ingestionJobId"),
            ingestion_job_id,
            "case {case:?}: the new job is a job of its own, got {restarted:?}"
        );
    }
}

/// RULE: what `StopIngestionJob` stops is "a currently running data ingestion
/// job", so a job that has reached ANY terminal status is refused with the
/// modelled `ValidationException` naming that status, and the stored job is left
/// exactly as it stands.
#[tokio::test]
async fn stopping_a_job_whose_run_is_over_is_refused() {
    let emulator = Emulator::new();

    // One row per terminal status a job can be in when the stop arrives: the run
    // that finished, and the run a stop already halted.
    let cases: [(&str, bool); 2] = [
        ("a job the clock completed", false),
        ("a job a stop already halted", true),
    ];

    for (index, (case, halt_first)) in cases.into_iter().enumerate() {
        let knowledge_base_id = emulator
            .knowledge_base(&format!("terminal-stop-kb-{index}"))
            .await;
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                s3_data_source(&format!("terminal-stop-source-{index}")),
            )
            .await;
        let started = emulator
            .start_ingestion_job(&knowledge_base_id, &data_source_id, json!({}))
            .await;
        assert_eq!(
            started.status, 202,
            "case {case:?}: expected StartIngestionJob -> 202, got {started:?}"
        );
        let ingestion_job_id = started.job_string("ingestionJobId");
        if halt_first {
            let halted = emulator
                .stop_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
                .await;
            assert_eq!(
                halted.status, 202,
                "case {case:?}: expected the first StopIngestionJob -> 202, got {halted:?}"
            );
        }
        let settled = emulator
            .poll_until_terminal(case, &knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        let terminal_status = settled.job_string("status");

        let refused = emulator
            .stop_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected the stop of a finished run to be refused 400, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!("Ingestion job is already in a terminal state: {terminal_status}."),
            "case {case:?}: the refusal names the status the job is in, got {:?}",
            refused.message()
        );

        // Nothing moved: the refused call left the stored job exactly as the
        // read before it reported.
        let after = emulator
            .get_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            after.body, settled.body,
            "case {case:?}: a refused stop must leave the stored job untouched"
        );
    }
}

/// RULE: a stop addresses a job by the knowledge base and data source it ran for
/// as well as its own id, so every part of that triple the store cannot resolve is
/// a missing resource.
#[tokio::test]
async fn stopping_a_job_nothing_holds_is_a_missing_resource() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("absent-stop-kb").await;
    let data_source_id = emulator
        .data_source(&knowledge_base_id, custom_data_source("absent-stop-source"))
        .await;
    let started = emulator
        .start_ingestion_job(&knowledge_base_id, &data_source_id, json!({}))
        .await;
    assert_eq!(
        started.status, 202,
        "case setup: expected StartIngestionJob -> 202, got {started:?}"
    );
    let ingestion_job_id = started.job_string("ingestionJobId");

    // The rows differ along WHICH part of the triple names nothing: an id of the
    // modelled `^[0-9a-zA-Z]{10}$` shape that no record carries, in each position.
    let absent = "0Absent000";
    let cases: [(&str, String, String, String); 3] = [
        (
            "a knowledge base nothing holds",
            absent.to_string(),
            data_source_id.clone(),
            ingestion_job_id.clone(),
        ),
        (
            "a data source nothing holds",
            knowledge_base_id.clone(),
            absent.to_string(),
            ingestion_job_id.clone(),
        ),
        (
            "a job nothing holds",
            knowledge_base_id.clone(),
            data_source_id.clone(),
            absent.to_string(),
        ),
    ];

    for (case, knowledge_base, data_source, job) in cases {
        let refused = emulator
            .stop_ingestion_job(&knowledge_base, &data_source, &job)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected the stop to be refused 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
    }
}

/// Read a job until its status is terminal, answering the last read AND every
/// status the reads observed on the way — the CONDITION is waited for, never a
/// fixed duration.
async fn poll_until_terminal_observing(
    emulator: &Emulator,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
    ingestion_job_id: &str,
) -> (Answer, Vec<String>) {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut observed: Vec<String> = Vec::new();
    loop {
        let answer = emulator
            .get_ingestion_job(knowledge_base_id, data_source_id, ingestion_job_id)
            .await;
        let status = answer.job_string("status");
        observed.push(status.clone());
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return (answer, observed);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, observed {observed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a halt dates the job it stops as the modelled members document —
/// `startedAt` stays "the time the job was started before the job was stopped"
/// and `updatedAt` becomes the time of the halt — so neither the stop nor the
/// clock finishing it moves `startedAt`.
#[tokio::test]
async fn a_stop_keeps_the_instant_the_job_started_and_stamps_the_halt() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the kind of data
    // source the halted job ingests, and which optional members its start
    // carried.
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "a job over a crawled data source, started with its required members only",
            s3_data_source("halt-stamp-documents"),
            json!({}),
        ),
        (
            "a job over a pushed data source, started with a description",
            custom_data_source("halt_stamp_documents"),
            json!({ "description": "the run the caller changed its mind about" }),
        ),
        (
            "a job over a crawled data source, started with a client token",
            s3_data_source("halt-stamp-documents-2"),
            json!({ "clientToken": "HaltStampIdempotencyTokenForTheThirdSource" }),
        ),
    ];

    for (index, (case, data_source_request, start_request)) in cases.into_iter().enumerate() {
        let knowledge_base_id = emulator
            .knowledge_base(&format!("halt-stamp-kb-{index}"))
            .await;
        let data_source_id = emulator
            .data_source(&knowledge_base_id, data_source_request)
            .await;
        let started = emulator
            .start_ingestion_job(&knowledge_base_id, &data_source_id, start_request)
            .await;
        assert_eq!(
            started.status, 202,
            "case {case:?}: expected StartIngestionJob -> 202, got {started:?}"
        );
        let ingestion_job_id = started.job_string("ingestionJobId");
        let started_at = started.job("startedAt").clone();
        let start_instant = started.job_instant("startedAt");
        let start_update_instant = started.job_instant("updatedAt");

        let stopped = emulator
            .stop_ingestion_job(&knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            stopped.status, 202,
            "case {case:?}: expected StopIngestionJob -> 202, got {stopped:?}"
        );
        assert_eq!(
            *stopped.job("startedAt"),
            started_at,
            "case {case:?}: a halted job reports the instant it started, not the instant \
             it was halted, got {:?}",
            stopped.job("startedAt")
        );
        let halt_instant = stopped.job_instant("updatedAt");
        assert!(
            halt_instant >= start_instant,
            "case {case:?}: expected updatedAt at or after startedAt {start_instant}, got \
             {stopped:?}"
        );
        assert!(
            halt_instant >= start_update_instant,
            "case {case:?}: the halt is the job's latest change, so expected updatedAt at or \
             after the start's {start_update_instant}, got {stopped:?}"
        );

        // The clock finishing the halt dates it the same way: the job still
        // started when it started, and its last change is no earlier than the
        // halt the stop stamped.
        let settled = emulator
            .poll_until_terminal(case, &knowledge_base_id, &data_source_id, &ingestion_job_id)
            .await;
        assert_eq!(
            settled.job_string("status"),
            "STOPPED",
            "case {case:?}: the clock owes a halted job only STOPPED, got {settled:?}"
        );
        assert_eq!(
            *settled.job("startedAt"),
            started_at,
            "case {case:?}: no later report moves the instant the job started, got {:?}",
            settled.job("startedAt")
        );
        assert!(
            settled.job_instant("updatedAt") >= halt_instant,
            "case {case:?}: expected updatedAt at or after the halt {halt_instant}, got \
             {settled:?}"
        );
    }
}

/// RULE: a stop halts only the job its knowledge base, data source and job
/// triple names — every other stored job keeps the run chain it was in, so none
/// of them ever reports a halted status and each still finishes `COMPLETE` with
/// its text queryable.
#[tokio::test]
async fn a_stop_halts_only_the_job_its_triple_names() {
    let emulator = Emulator::new();
    let first_knowledge_base = emulator.knowledge_base("confined-stop-kb-0").await;
    let second_knowledge_base = emulator.knowledge_base("confined-stop-kb-1").await;
    let halted_source = emulator
        .data_source(&first_knowledge_base, s3_data_source("confined-halted-source"))
        .await;
    let sibling_source = emulator
        .data_source(
            &first_knowledge_base,
            custom_data_source("confined_sibling_source"),
        )
        .await;
    let other_base_source = emulator
        .data_source(
            &second_knowledge_base,
            s3_data_source("confined-other-base-source"),
        )
        .await;

    // The rows differ along where the job the stop must not reach lives
    // relative to the halted one: another data source of the SAME knowledge
    // base, and a data source of ANOTHER knowledge base.
    let siblings: [(&str, &str, &str); 2] = [
        (
            "a job of another data source of the same knowledge base",
            first_knowledge_base.as_str(),
            sibling_source.as_str(),
        ),
        (
            "a job of a data source of another knowledge base",
            second_knowledge_base.as_str(),
            other_base_source.as_str(),
        ),
    ];
    let mut sibling_jobs: Vec<(&str, &str, &str, String)> = Vec::new();
    for (case, knowledge_base_id, data_source_id) in siblings {
        let started = emulator
            .start_ingestion_job(knowledge_base_id, data_source_id, json!({}))
            .await;
        assert_eq!(
            started.status, 202,
            "case {case:?}: expected StartIngestionJob -> 202, got {started:?}"
        );
        sibling_jobs.push((
            case,
            knowledge_base_id,
            data_source_id,
            started.job_string("ingestionJobId"),
        ));
    }

    // The job to halt is started LAST, so the stop addresses a run that has
    // only just begun however long the setup before it took.
    let halted = emulator
        .start_ingestion_job(&first_knowledge_base, &halted_source, json!({}))
        .await;
    assert_eq!(
        halted.status, 202,
        "case setup: expected StartIngestionJob -> 202, got {halted:?}"
    );
    let halted_job = halted.job_string("ingestionJobId");
    let stopped = emulator
        .stop_ingestion_job(&first_knowledge_base, &halted_source, &halted_job)
        .await;
    assert_eq!(
        stopped.status, 202,
        "case the halted job: expected StopIngestionJob -> 202, got {stopped:?}"
    );
    assert_eq!(
        stopped.job_string("status"),
        "STOPPING",
        "case the halted job: the job the triple named is stopping, got {stopped:?}"
    );

    for (case, knowledge_base_id, data_source_id, ingestion_job_id) in &sibling_jobs {
        let (settled, observed) = poll_until_terminal_observing(
            &emulator,
            case,
            knowledge_base_id,
            data_source_id,
            ingestion_job_id,
        )
        .await;
        for status in &observed {
            assert!(
                !HALT_STATUSES.contains(&status.as_str()),
                "case {case:?}: a job no stop named is never halted, observed {observed:?}"
            );
        }
        assert_eq!(
            settled.job_string("status"),
            "COMPLETE",
            "case {case:?}: a run nothing halted finishes, got {settled:?}"
        );
        assert_eq!(
            settled.job_string("ingestionJobId"),
            *ingestion_job_id,
            "case {case:?}: the read reports the job the URI named, got {settled:?}"
        );
        assert!(
            !settled.job("textReadyAt").is_null(),
            "case {case:?}: a finished run's text became queryable, got {settled:?}"
        );
    }

    let settled_halt = emulator
        .poll_until_terminal(
            "the halted job",
            &first_knowledge_base,
            &halted_source,
            &halted_job,
        )
        .await;
    assert_eq!(
        settled_halt.job_string("status"),
        "STOPPED",
        "case the halted job: the clock owes a halted job only STOPPED, got {settled_halt:?}"
    );
    assert!(
        settled_halt.job("textReadyAt").is_null(),
        "case the halted job: a halted run extracted no text, got {:?}",
        settled_halt.job("textReadyAt")
    );
}
