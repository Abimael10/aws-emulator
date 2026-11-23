// SPDX-License-Identifier: CC0-1.0

//! `ListIngestionJobs` WHILE the clock is still moving the jobs it reports: the
//! listing is a projection of the ingestion jobs the addressed data source holds
//! at the instant of the read, never a value of its own.
//!
//! The rule this suite pins, over tables rather than one example:
//!
//! * at every stage of a job's clock-owed lifecycle the summary the listing
//!   reports for that job carries the values a `GetIngestionJob` of the very
//!   same job reports, so the two readers never disagree about one record;
//! * the listing reports one summary per start the service ACCEPTED, in the
//!   order it accepted them — a start it refused, and a start replayed under an
//!   idempotency token it has already served, add nothing to it;
//! * listing is a read: it neither advances nor regresses a job, so a reported
//!   status only ever moves forward along the modelled `IngestionJobStatus`
//!   lifecycle and a listing of jobs the clock owes nothing is identical however
//!   many times it is asked for.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly, and every wait is a poll for the CONDITION the
//! service owes rather than a fixed sleep.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The vector bucket the knowledge bases here bind to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/job-progress-vectors";

/// The index of [`VECTOR_BUCKET`] the knowledge bases here bind to.
const INDEX: &str = "job-progress-index";

/// How long a poll waits for a condition the CLOCK owes: many times the spans
/// this service owes, so a loaded machine still converges, and bounded so a hung
/// poll fails the test rather than hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before reading again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The members of the modelled `IngestionJobSummary` whose value is fixed when
/// the job is started, so the two readers must agree on them at every instant.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="IngestionJobSummary")`).
const SETTLED_MEMBERS: [&str; 5] = [
    "knowledgeBaseId",
    "dataSourceId",
    "ingestionJobId",
    "description",
    "startedAt",
];

/// Every member of the modelled `IngestionJobSummary`: the settled ones plus the
/// members the job's own progress moves, which the two readers must agree on
/// whenever they report the job in the same status.
///
/// Source: as [`SETTLED_MEMBERS`].
const SUMMARY_MEMBERS: [&str; 9] = [
    "knowledgeBaseId",
    "dataSourceId",
    "ingestionJobId",
    "description",
    "status",
    "startedAt",
    "updatedAt",
    "textReadyAt",
    "statistics",
];

/// The `IngestionJobStatus` members a started job of this emulator passes
/// through, in the order it passes through them — the order a status may only
/// move forward along.
///
/// Source: the `IngestionJobStatus` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="IngestionJobStatus")`), whose remaining
/// members (`FAILED`, `STOPPING`, `STOPPED`) no job here reaches unless it is
/// stopped.
const LIFECYCLE: [&str; 3] = ["STARTING", "IN_PROGRESS", "COMPLETE"];

/// The terminal status of the lifecycle a started job runs to.
const TERMINAL: &str = "COMPLETE";

/// An idempotency token of the modelled `ClientToken` form (length 33..256,
/// pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`), at its shortest admitted
/// length.
///
/// Source: `StartIngestionJob`'s `clientToken` member of this service's Smithy
/// model (`smithy_query(query="operation",operation="StartIngestionJob")`).
const TOKEN_SHORTEST: &str = "JobProgressTokenForTheFirstReplay";

/// Another token of the same modelled form, spelt with the hyphens the pattern
/// admits between its alphanumerics.
const TOKEN_HYPHENATED: &str = "Job-Progress-Replay-Token-Number2";

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
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
            VECTOR_BUCKET,
            INDEX,
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
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
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

    /// Create one knowledge base and report its minted id.
    async fn knowledge_base(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "VECTOR",
                        "vectorKnowledgeBaseConfiguration": {
                            "embeddingModelArn": EMBEDDING_MODEL
                        },
                    },
                    "storageConfiguration": {
                        "type": "S3_VECTORS",
                        "s3VectorsConfiguration": {
                            "vectorBucketArn": VECTOR_BUCKET,
                            "indexName": INDEX,
                        },
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case {case}: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.string(&["knowledgeBase", "knowledgeBaseId"])
    }

    /// Attach one CUSTOM data source to a knowledge base and report its minted
    /// id.
    async fn data_source(&self, case: &str, knowledge_base_id: &str, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(json!({
                    "name": name,
                    "dataSourceConfiguration": { "type": "CUSTOM" },
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected CreateDataSource -> 200, got {created:?}"
        );
        created.string(&["dataSource", "dataSourceId"])
    }

    /// `StartIngestionJob`, exactly as a client sends it — whatever the service
    /// answers.
    async fn start(&self, knowledge_base_id: &str, data_source_id: &str, request: Value) -> Answer {
        self.call(
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/"
            ),
            Some(request),
        )
        .await
    }

    /// Start one job the service must accept, reporting its minted id.
    async fn start_accepted(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) -> String {
        let started = self.start(knowledge_base_id, data_source_id, request).await;
        assert_eq!(
            started.status, 202,
            "case {case}: expected StartIngestionJob -> 202, got {started:?}"
        );
        started.string(&["ingestionJob", "ingestionJobId"])
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

    /// `ListIngestionJobs` over the whole listing: its summaries, asserting the
    /// modelled success and that one page reported everything.
    async fn whole_listing(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
    ) -> Vec<Value> {
        let listed = self
            .call(
                "POST",
                &format!(
                    "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                     /ingestionjobs/"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListIngestionJobs -> 200, got {listed:?}"
        );
        assert!(
            listed.member(&["nextToken"]).is_null(),
            "case {case}: a page that reported everything hands out no token, got {:?}",
            listed.member(&["nextToken"])
        );
        listed
            .member(&["ingestionJobSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: ingestionJobSummaries is a required list, got {:?}",
                    listed.body
                )
            })
            .clone()
    }
}

/// The `ingestionJobId` of one summary.
fn summary_id(case: &str, summary: &Value) -> String {
    summary["ingestionJobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a summary carries an ingestionJobId, {summary:?}"))
        .to_string()
}

/// The ids a page of summaries reported, in the order it reported them.
fn ids(case: &str, summaries: &[Value]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| summary_id(case, summary))
        .collect()
}

/// The `status` one summary reported.
fn status_of(case: &str, summary: &Value) -> String {
    summary["status"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a summary carries a status, {summary:?}"))
        .to_string()
}

/// How far along [`LIFECYCLE`] a reported status is.
fn stage_of(case: &str, status: &str) -> usize {
    LIFECYCLE
        .iter()
        .position(|member| *member == status)
        .unwrap_or_else(|| {
            panic!(
                "case {case}: a started job holds one of the statuses {LIFECYCLE:?}, \
                 got {status:?}"
            )
        })
}

/// Assert the summary the listing reported for one job and a `GetIngestionJob`
/// of that same job describe ONE record, and report how far along the lifecycle
/// each of the two reads saw it.
async fn agree_on_the_job(
    emulator: &Emulator,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
    summary: &Value,
) -> (usize, usize) {
    let ingestion_job_id = summary_id(case, summary);
    let listed_status = status_of(case, summary);
    let fetched = emulator
        .get_ingestion_job(knowledge_base_id, data_source_id, &ingestion_job_id)
        .await;
    assert_eq!(
        fetched.status, 200,
        "case {case}: expected GetIngestionJob({ingestion_job_id}) -> 200, got {fetched:?}"
    );
    for member in SETTLED_MEMBERS {
        assert_eq!(
            &summary[member],
            fetched.member(&["ingestionJob", member]),
            "case {case}: job {ingestion_job_id}'s summary member {member} must be the \
             stored value the read of that job reports, got {:?}",
            summary[member]
        );
    }
    let fetched_status = fetched
        .member(&["ingestionJob", "status"])
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the read reports a status, got {fetched:?}"))
        .to_string();
    // The clock may have moved the job on between the two reads, so only a pair
    // of reads that saw the same status has to agree on the members that status
    // moves — and the later read can only ever have seen it further along.
    if fetched_status == listed_status {
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                &summary[member],
                fetched.member(&["ingestionJob", member]),
                "case {case}: job {ingestion_job_id} is in status {listed_status} for both \
                 readers, so its {member} must be the same stored value, got {:?}",
                summary[member]
            );
        }
    }
    (
        stage_of(case, &listed_status),
        stage_of(case, &fetched_status),
    )
}

/// The listing tracks each job through its whole clock-owed lifecycle, reporting
/// at every stage the record a read of that job reports, and moving a status
/// only forward — over data sources holding one, two and three jobs.
#[tokio::test]
async fn the_listing_tracks_every_job_as_a_read_of_that_job_does() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the tracked store", "job-progress-kb")
        .await;

    // Rows differ along what the rule generalises over: how many jobs the data
    // source has run by the time the listing is read, and so how many records
    // one listing tracks at once.
    for job_count in 1..=3_usize {
        let row = format!("a data source running {job_count} job(s)");
        let data_source_id = emulator
            .data_source(&row, &knowledge_base_id, &format!("job-progress-{job_count}"))
            .await;

        // A data source that has run no job has nothing to report.
        let empty = emulator
            .whole_listing(&row, &knowledge_base_id, &data_source_id)
            .await;
        assert!(
            empty.is_empty(),
            "case {row}: a data source that has started no job reports no summary, got \
             {empty:?}"
        );

        let mut accepted: Vec<String> = Vec::new();
        // The furthest along the lifecycle each job has been SEEN, which no
        // later read may report it behind.
        let mut furthest: BTreeMap<String, usize> = BTreeMap::new();

        for position in 0..job_count {
            // A data source ingests one job at a time, so each job here is
            // started once the one before it has finished — and the listing is
            // read all the way through this job's own progress.
            let case = format!("{row}, job #{position}");
            let ingestion_job_id = emulator
                .start_accepted(
                    &case,
                    &knowledge_base_id,
                    &data_source_id,
                    json!({ "description": format!("crawl #{position}") }),
                )
                .await;
            accepted.push(ingestion_job_id.clone());

            let deadline = Instant::now() + POLL_CAP;
            loop {
                let summaries = emulator
                    .whole_listing(&case, &knowledge_base_id, &data_source_id)
                    .await;
                let listed = ids(&case, &summaries);
                assert_eq!(
                    listed, accepted,
                    "case {case}: the listing reports one summary per accepted start, in the \
                     order they were accepted, got {listed:?}"
                );

                let mut running_status = String::new();
                for summary in &summaries {
                    let job_id = summary_id(&case, summary);
                    let (listed_stage, fetched_stage) = agree_on_the_job(
                        &emulator,
                        &case,
                        &knowledge_base_id,
                        &data_source_id,
                        summary,
                    )
                    .await;
                    let seen = furthest.entry(job_id.clone()).or_insert(listed_stage);
                    assert!(
                        listed_stage >= *seen,
                        "case {case}: job {job_id} was already reported at \
                         {:?}, so no later read may put it back at {:?}",
                        LIFECYCLE[*seen],
                        LIFECYCLE[listed_stage]
                    );
                    assert!(
                        fetched_stage >= listed_stage,
                        "case {case}: job {job_id} was listed at {:?}, so the read that \
                         followed cannot report it behind that, got {:?}",
                        LIFECYCLE[listed_stage],
                        LIFECYCLE[fetched_stage]
                    );
                    *seen = listed_stage.max(fetched_stage);
                    if job_id == ingestion_job_id {
                        running_status = status_of(&case, summary);
                    }
                }

                if running_status == TERMINAL {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case}: the clock must take job {ingestion_job_id} to {TERMINAL}, \
                     the listing last reported it at {running_status:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }

        // Every job the data source ran is reported, once, in the order the
        // starts were accepted, each at the terminal status the clock owed it —
        // and the listing of a store the clock owes nothing is the same answer
        // every time it is asked for.
        let settled = emulator
            .whole_listing(&row, &knowledge_base_id, &data_source_id)
            .await;
        assert_eq!(
            ids(&row, &settled),
            accepted,
            "case {row}: expected every accepted job once, in start order, got {settled:?}"
        );
        for summary in &settled {
            assert_eq!(
                status_of(&row, summary),
                TERMINAL,
                "case {row}: a job the clock has finished is reported at {TERMINAL}, got \
                 {summary:?}"
            );
        }
        let mut previous_started_at = f64::MIN;
        for summary in &settled {
            let started_at = crate::update_round_trip::date_time_wire_instant(
                &row,
                "a summary's startedAt",
                &summary["startedAt"],
            );
            assert!(
                started_at >= previous_started_at,
                "case {row}: the default order is when the job started, so the reported \
                 startedAt never goes back, got {started_at} after {previous_started_at}"
            );
            previous_started_at = started_at;
        }
        for repeat in 0..3 {
            let again = emulator
                .whole_listing(&row, &knowledge_base_id, &data_source_id)
                .await;
            assert_eq!(
                again, settled,
                "case {row}, read #{repeat}: listing is a read, so it must not change what a \
                 later caller sees, got {again:?}"
            );
        }
    }
}

/// The listing reports one summary per start the service ACCEPTED: a refused
/// start and a start replayed under an idempotency token already served add
/// nothing to it.
#[tokio::test]
async fn the_listing_reports_only_the_starts_the_service_accepted() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the accepted store", "job-accepted-kb")
        .await;
    let data_source_id = emulator
        .data_source("the accepted source", &knowledge_base_id, "job-accepted-src")
        .await;

    // Every start the service answered 202 to, in the order it answered — which
    // is exactly what the listing must report, after every row below.
    let mut accepted: Vec<String> = vec![
        emulator
            .start_accepted("the first start", &knowledge_base_id, &data_source_id, json!({}))
            .await,
    ];

    // A second start while the first job may still be running: a data source is
    // ingested by one job at a time, so the service either refuses this one as
    // the modelled conflict or — if the clock has already finished the first —
    // accepts it as a job of its own. Either way the listing reports exactly
    // the starts it accepted.
    let second = emulator
        .start(&knowledge_base_id, &data_source_id, json!({}))
        .await;
    match second.status {
        202 => accepted.push(second.string(&["ingestionJob", "ingestionJobId"])),
        409 => assert!(
            second
                .error_type
                .as_deref()
                .is_some_and(|error_type| error_type.contains("ConflictException")),
            "case a second start of the same data source: expected the modelled \
             ConflictException, got {second:?}"
        ),
        _ => panic!(
            "case a second start of the same data source: expected either the modelled 202 \
             or the modelled 409 conflict, got {second:?}"
        ),
    }
    let listed = emulator
        .whole_listing(
            "after a second start of the same data source",
            &knowledge_base_id,
            &data_source_id,
        )
        .await;
    assert_eq!(
        ids("after a second start of the same data source", &listed),
        accepted,
        "case after a second start of the same data source: expected only the starts the \
         service accepted, got {listed:?}"
    );

    // Rows differ along what the rule generalises over: the form of the modelled
    // `ClientToken` the start carried, and whether the request beside it carried
    // a description — a replay of any of them is the job it already started, so
    // it adds no second summary.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a token at its shortest modelled length",
            json!({ "clientToken": TOKEN_SHORTEST }),
        ),
        (
            "a hyphenated token, with a description",
            json!({ "clientToken": TOKEN_HYPHENATED, "description": "the replayed crawl" }),
        ),
    ];

    for (case, request) in rows {
        // Each job starts once the one before it has finished, because a data
        // source ingests one at a time.
        poll_until_nothing_runs(&emulator, case, &knowledge_base_id, &data_source_id).await;
        let first = emulator
            .start_accepted(case, &knowledge_base_id, &data_source_id, request.clone())
            .await;
        accepted.push(first.clone());

        for replay in 0..2 {
            let replayed = emulator
                .start(&knowledge_base_id, &data_source_id, request.clone())
                .await;
            assert_eq!(
                replayed.status, 202,
                "case {case}, replay #{replay}: a repeat of a served idempotency token is \
                 served again, got {replayed:?}"
            );
            assert_eq!(
                replayed.string(&["ingestionJob", "ingestionJobId"]),
                first,
                "case {case}, replay #{replay}: a repeat under the same token names the job \
                 it already started, got {replayed:?}"
            );
            let listed = emulator
                .whole_listing(case, &knowledge_base_id, &data_source_id)
                .await;
            assert_eq!(
                ids(case, &listed),
                accepted,
                "case {case}, replay #{replay}: a replayed start adds no second summary, got \
                 {listed:?}"
            );
        }
    }
}

/// Poll the listing until no job of this data source is still running, so the
/// next start is one the service accepts rather than the conflict it models.
async fn poll_until_nothing_runs(
    emulator: &Emulator,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
) {
    let deadline = Instant::now() + POLL_CAP;
    loop {
        let summaries = emulator
            .whole_listing(case, knowledge_base_id, data_source_id)
            .await;
        let running: Vec<String> = summaries
            .iter()
            .filter(|summary| status_of(case, summary) != TERMINAL)
            .map(|summary| summary_id(case, summary))
            .collect();
        if running.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the clock must finish every running job, still running {running:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
