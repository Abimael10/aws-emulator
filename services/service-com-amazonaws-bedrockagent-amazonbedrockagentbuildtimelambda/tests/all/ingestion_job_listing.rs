// SPDX-License-Identifier: CC0-1.0

//! `ListIngestionJobs` end to end over HTTP: the listing is a PROJECTION of the
//! ingestion jobs one data source's store holds, never a value of its own.
//!
//! The rule this suite pins, over tables rather than one example:
//!
//! * every ingestion job of the addressed data source is reported exactly once,
//!   as the modelled `IngestionJobSummary` — each member carrying the same
//!   stored value `GetIngestionJob` reports for that job — and no other data
//!   source's job is;
//! * `maxResults` shortens the page and the `nextToken` it hands out resumes
//!   exactly after it, in whichever order the request asked for, so the pages
//!   together are the whole listing once;
//! * `filters` reports only the jobs the filter admits, and `sortBy` orders the
//!   same set by the field and direction it names;
//! * a knowledge base or data source the store does not hold — including one a
//!   delete has already been accepted for — has nothing to enumerate and is
//!   refused as the missing resource, and a `nextToken` this service never
//!   issued names no position.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather
//! than calling a handler directly, and every wait is a poll for the CONDITION
//! the service owes rather than a fixed sleep.

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
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/job-listing-vectors";

/// The index of [`VECTOR_BUCKET`] the knowledge bases here bind to.
const INDEX: &str = "job-listing-index";

/// How long a poll waits for a condition the CLOCK owes: many times the spans
/// this service owes, so a loaded machine still converges, and bounded so a
/// hung poll fails the test rather than hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before reading again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Every member of the modelled `IngestionJobSummary`, and the only members a
/// summary may carry.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="IngestionJobSummary")`).
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

/// The `IngestionJobStatus` members a job of this emulator can report.
///
/// Source: the `IngestionJobStatus` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="IngestionJobStatus")`), of which these
/// are the three a started job passes through.
const REACHABLE_STATUSES: [&str; 3] = ["STARTING", "IN_PROGRESS", "COMPLETE"];

/// `IngestionJobStatus` members no job here ever reaches, so a filter naming
/// only these admits nothing: a job is stopped by `StopIngestionJob`, and no
/// job of this emulator fails.
///
/// Source: as [`REACHABLE_STATUSES`].
const UNREACHED_STATUSES: [&str; 3] = ["FAILED", "STOPPING", "STOPPED"];

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

    /// The error message the service worded, as a client reads it.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
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

    async fn delete_data_source(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "DELETE",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
            None,
        )
        .await
    }

    async fn get_data_source(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "GET",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
            None,
        )
        .await
    }

    /// Read the data source until it answers the modelled 404 — the removal an
    /// accepted delete put on the clock, which a client converges on by polling
    /// exactly this way.
    async fn poll_data_source_until_gone(&self, knowledge_base_id: &str, data_source_id: &str) {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let read = self.get_data_source(knowledge_base_id, data_source_id).await;
            if read.status == 404 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case setup: the clock must remove data source {data_source_id}, \
                 last read {read:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
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

    /// Start one ingestion job and report its minted id.
    async fn start_ingestion_job(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) -> String {
        let started = self
            .call(
                "PUT",
                &format!(
                    "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                     /ingestionjobs/"
                ),
                Some(request),
            )
            .await;
        assert_eq!(
            started.status, 202,
            "case {case}: expected StartIngestionJob -> 202, got {started:?}"
        );
        started.string(&["ingestionJob", "ingestionJobId"])
    }

    /// Start one job and POLL `GetIngestionJob` until the clock has completed
    /// it, reporting its id: a data source ingests one job at a time, so a
    /// second job of the same data source starts only once this one is done.
    async fn run_ingestion_job(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) -> String {
        let ingestion_job_id = self
            .start_ingestion_job(case, knowledge_base_id, data_source_id, request)
            .await;
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let read = self
                .get_ingestion_job(knowledge_base_id, data_source_id, &ingestion_job_id)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetIngestionJob -> 200, got {read:?}"
            );
            if read.string(&["ingestionJob", "status"]) == "COMPLETE" {
                return ingestion_job_id;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: the clock must complete job {ingestion_job_id}, \
                 last read {read:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `ListIngestionJobs`, exactly as a client sends it.
    async fn list(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        body: Value,
    ) -> Answer {
        self.call(
            "POST",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/"
            ),
            Some(body),
        )
        .await
    }

    /// One served page: its summaries and the token it hands out.
    async fn page(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        body: Value,
    ) -> (Vec<Value>, Option<String>) {
        let listed = self.list(knowledge_base_id, data_source_id, body).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListIngestionJobs -> 200, got {listed:?}"
        );
        let summaries = listed
            .member(&["ingestionJobSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: ingestionJobSummaries is a required list, got {:?}",
                    listed.body
                )
            })
            .clone();
        let token = listed
            .member(&["nextToken"])
            .as_str()
            .map(ToString::to_string);
        (summaries, token)
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

/// One timestamp member of a summary, rendered so that comparing the renderings
/// compares the instants. The member is on the wire in the modelled `date-time`
/// form, so the rendering is the instant it names, at a fixed width.
fn instant_key(value: &Value) -> String {
    let seconds = crate::update_round_trip::date_time_wire_instant(
        "the sorted listing",
        "a summary's startedAt",
        value,
    );
    format!("{seconds:030.9}")
}

/// A `filters` entry selecting jobs whose status equals one of `values`: the
/// one attribute and the one operator the model declares
/// (`smithy_query(query="shape",shape="IngestionJobFilterAttribute,IngestionJobFilterOperator")`).
fn status_filter(values: &[&str]) -> Value {
    json!([{ "attribute": "STATUS", "operator": "EQ", "values": values }])
}

/// Walk the whole listing `page_size` at a time under `body`'s filters and
/// ordering, reporting the ids the pages carried and asserting the paging
/// invariants on every page.
async fn walk(
    emulator: &Emulator,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
    body: &Value,
    page_size: Option<i64>,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page per stored job is the most a terminating walk needs; past that
    // the token is not advancing and the test must fail rather than loop.
    for _ in 0..64 {
        let mut request = body.clone();
        if let Some(page_size) = page_size {
            request["maxResults"] = json!(page_size);
        }
        if let Some(token) = token.as_deref() {
            request["nextToken"] = json!(token);
        }
        let (summaries, next) = emulator
            .page(case, knowledge_base_id, data_source_id, request)
            .await;
        if let Some(page_size) = page_size {
            let page_size = usize::try_from(page_size).unwrap_or(usize::MAX);
            assert!(
                summaries.len() <= page_size,
                "case {case}: a page carries at most the maxResults asked for \
                 ({page_size}), got {} summaries",
                summaries.len()
            );
            assert!(
                next.is_none() || summaries.len() == page_size,
                "case {case}: a page that hands out a token is a full page, got {} summaries",
                summaries.len()
            );
        }
        seen.extend(ids(case, &summaries));
        token = next;
        if token.is_none() {
            return seen;
        }
    }
    panic!("case {case}: the walk did not terminate after 64 pages, saw {seen:?}");
}

/// The listing reports a summary of every ingestion job of the addressed data
/// source, once each, in the order they started, with every member carrying the
/// value `GetIngestionJob` reports for that job — and nothing of another data
/// source's.
#[tokio::test]
async fn the_listing_reports_a_summary_of_every_ingestion_job_of_the_data_source() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("the listed store", "job-listing-kb").await;
    let data_source_id = emulator
        .data_source("the listed source", &knowledge_base_id, "job-listing-source")
        .await;
    let other_data_source_id = emulator
        .data_source("the other source", &knowledge_base_id, "job-listing-other")
        .await;

    // A data source that has run no job has nothing to report and nowhere to
    // resume.
    let (empty, token) = emulator
        .page(
            "a data source with no job",
            &knowledge_base_id,
            &data_source_id,
            json!({}),
        )
        .await;
    assert!(
        empty.is_empty() && token.is_none(),
        "case a data source with no job: expected no summary and no nextToken, got \
         {empty:?}/{token:?}"
    );

    // Rows differ along what the rule generalises over: which optional members
    // the start request carried, and the form of the modelled `Description`
    // (`length 1..200`) — absent, one character, a sentence, and its longest
    // form.
    let rows: Vec<(&str, Value)> = vec![
        ("started with only its required members", json!({})),
        (
            "started with a description",
            json!({ "description": "the first crawl of the demo source" }),
        ),
        (
            "started with a one-character description and a client token",
            json!({ "description": "d", "clientToken": "JobListingClientTokenForTheFirstJob" }),
        ),
        (
            "started with a description at its longest form",
            json!({ "description": "d".repeat(200) }),
        ),
    ];

    let mut started: Vec<(&str, String)> = Vec::new();
    for (case, request) in &rows {
        let ingestion_job_id = emulator
            .run_ingestion_job(case, &knowledge_base_id, &data_source_id, request.clone())
            .await;
        started.push((case, ingestion_job_id));
    }
    // One job of ANOTHER data source, which this listing must never report.
    let foreign = emulator
        .run_ingestion_job(
            "the other source's job",
            &knowledge_base_id,
            &other_data_source_id,
            json!({}),
        )
        .await;

    let (summaries, token) = emulator
        .page(
            "the whole listing",
            &knowledge_base_id,
            &data_source_id,
            json!({}),
        )
        .await;
    let listed = ids("the whole listing", &summaries);
    let expected: Vec<String> = started.iter().map(|(_, id)| id.clone()).collect();
    assert_eq!(
        listed, expected,
        "case the whole listing: expected exactly this data source's jobs, in the order \
         they started, got {listed:?}"
    );
    assert!(
        !listed.contains(&foreign),
        "case the whole listing: another data source's job is in no listing of this one, \
         got {listed:?}"
    );
    assert!(
        token.is_none(),
        "case the whole listing: a page that reported everything hands out no token, got \
         {token:?}"
    );

    // Each summary is the same stored record a read of that job reports,
    // projected onto the modelled summary members.
    for ((case, ingestion_job_id), summary) in started.iter().zip(summaries.iter()) {
        let fetched = emulator
            .get_ingestion_job(&knowledge_base_id, &data_source_id, ingestion_job_id)
            .await;
        assert_eq!(
            fetched.status, 200,
            "case {case}: expected GetIngestionJob -> 200, got {fetched:?}"
        );
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                &summary[member],
                fetched.member(&["ingestionJob", member]),
                "case {case}: the summary's {member} must be the stored value the read \
                 reports, got {:?}",
                summary[member]
            );
        }
        let extra: Vec<&String> = summary
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: a summary is a structure, got {summary:?}"))
            .keys()
            .filter(|member| !SUMMARY_MEMBERS.contains(&member.as_str()))
            .collect();
        assert!(
            extra.is_empty(),
            "case {case}: a summary carries only the modelled IngestionJobSummary members, \
             got {extra:?} as well"
        );
    }

    // Listing is a read: it changes nothing a later caller sees. Every job here
    // has reached the terminal status, so the clock owes none of them a change.
    let (again, _) = emulator
        .page(
            "a repeated listing",
            &knowledge_base_id,
            &data_source_id,
            json!({}),
        )
        .await;
    assert_eq!(
        again, summaries,
        "case a repeated listing: a read must not change what a later caller sees, got \
         {again:?}"
    );
}

/// `maxResults` shortens the page and the token resumes exactly after it: the
/// pages together are the whole listing once, whatever the page size — over a
/// table of page sizes against a table of stored counts.
#[tokio::test]
async fn max_results_pages_the_listing_without_losing_or_repeating_one() {
    // Rows: the stored count spans a data source that has run none, one and
    // several; the page sizes span one, a divisor of the count, a size the
    // count does not divide by, exactly the count, one more, and each published
    // bound of the modelled `maxResults` range (1..1000).
    for count in [0_usize, 1, 3] {
        let emulator = Emulator::new();
        let knowledge_base_id = emulator
            .knowledge_base("the paged store", &format!("job-paging-kb-{count}"))
            .await;
        let data_source_id = emulator
            .data_source(
                "the paged source",
                &knowledge_base_id,
                &format!("job-paging-source-{count}"),
            )
            .await;
        let mut stored = Vec::with_capacity(count);
        for position in 0..count {
            stored.push(
                emulator
                    .run_ingestion_job(
                        &format!("the job #{position}"),
                        &knowledge_base_id,
                        &data_source_id,
                        json!({}),
                    )
                    .await,
            );
        }

        for page_size in [None, Some(1), Some(2), Some(3), Some(4), Some(1000)] {
            let case = format!("{count} stored, maxResults={page_size:?}");
            let walked = walk(
                &emulator,
                &case,
                &knowledge_base_id,
                &data_source_id,
                &json!({}),
                page_size,
            )
            .await;
            assert_eq!(
                walked, stored,
                "case {case}: the pages together are the whole listing once, in the order \
                 the jobs started, got {walked:?}"
            );
        }

        // The pages are cut from one order, so the first page of any size is
        // the head of the whole listing.
        for page_size in [1_i64, 2, 3, 1000] {
            let case = format!("{count} stored, the first page of {page_size}");
            let (summaries, _) = emulator
                .page(
                    &case,
                    &knowledge_base_id,
                    &data_source_id,
                    json!({ "maxResults": page_size }),
                )
                .await;
            let head = ids(&case, &summaries);
            let wanted: Vec<String> = stored
                .iter()
                .take(usize::try_from(page_size).unwrap_or(usize::MAX))
                .cloned()
                .collect();
            assert_eq!(
                head, wanted,
                "case {case}: the first page is the head of the listing, got {head:?}"
            );
        }

        // A listing in the reverse direction pages the same set the other way
        // round, and its token resumes in that order too.
        let descending = json!({
            "sortBy": { "attribute": "STARTED_AT", "order": "DESCENDING" }
        });
        let mut reversed = stored.clone();
        reversed.reverse();
        for page_size in [None, Some(1), Some(2)] {
            let case = format!("{count} stored, descending, maxResults={page_size:?}");
            let walked = walk(
                &emulator,
                &case,
                &knowledge_base_id,
                &data_source_id,
                &descending,
                page_size,
            )
            .await;
            assert_eq!(
                walked, reversed,
                "case {case}: the pages of a descending listing are the whole listing once, \
                 newest first, got {walked:?}"
            );
        }
    }
}

/// `filters` reports only the jobs the filter admits: over a table of filters
/// against a data source whose jobs have all reached the terminal status, so
/// what each filter admits is fixed, and against one holding a job the clock
/// has not finished.
#[tokio::test]
async fn filters_report_only_the_jobs_the_filter_admits() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the filtered store", "job-filter-kb")
        .await;
    let finished_source_id = emulator
        .data_source("the finished source", &knowledge_base_id, "job-filter-source")
        .await;
    let running_source_id = emulator
        .data_source("the running source", &knowledge_base_id, "job-filter-running")
        .await;

    let mut finished: Vec<String> = Vec::new();
    for position in 0..2 {
        finished.push(
            emulator
                .run_ingestion_job(
                    &format!("the finished job #{position}"),
                    &knowledge_base_id,
                    &finished_source_id,
                    json!({}),
                )
                .await,
        );
    }

    // Rows differ along what the rule generalises over: how many values the
    // filter carries (none, one, several, its modelled maximum of 10), whether
    // they name the status the stored jobs hold, and the spelling of a value.
    // Ten values: the three statuses a started job passes through, the three
    // modelled members no job here reaches, and four strings no
    // `IngestionJobStatus` declares.
    let mut ten_values: Vec<&str> = REACHABLE_STATUSES.to_vec();
    ten_values.extend(UNREACHED_STATUSES);
    ten_values.extend(["", "COMPLETED", "complete", "COMPLETE "]);
    let all: Vec<String> = finished.clone();
    let none: Vec<String> = Vec::new();
    let rows: Vec<(&str, Vec<&str>, Vec<String>)> = vec![
        (
            "every status a started job passes through",
            REACHABLE_STATUSES.to_vec(),
            all.clone(),
        ),
        ("only the terminal status", vec!["COMPLETE"], all.clone()),
        (
            "only statuses no job here reaches",
            UNREACHED_STATUSES.to_vec(),
            none.clone(),
        ),
        ("no value at all", vec![], none.clone()),
        (
            "a status in the wrong case",
            vec!["complete", "Complete"],
            none.clone(),
        ),
        ("ten values, the modelled maximum", ten_values, all.clone()),
    ];

    for (case, values, expected) in rows {
        let (summaries, _) = emulator
            .page(
                case,
                &knowledge_base_id,
                &finished_source_id,
                json!({ "filters": status_filter(&values) }),
            )
            .await;
        let listed = ids(case, &summaries);
        assert_eq!(
            listed, expected,
            "case {case} (values {values:?}): expected exactly the jobs the filter admits, \
             got {listed:?}"
        );
        for summary in &summaries {
            let status = summary["status"].as_str().unwrap_or_default();
            assert!(
                values.contains(&status),
                "case {case}: a listed job's status is one the filter named, got {status:?}"
            );
        }
    }

    // A filter and a page size compose: the filtered listing pages like any
    // other.
    let case = "the terminal status, one job per page";
    let walked = walk(
        &emulator,
        case,
        &knowledge_base_id,
        &finished_source_id,
        &json!({ "filters": status_filter(&["COMPLETE"]) }),
        Some(1),
    )
    .await;
    assert_eq!(
        walked, finished,
        "case {case}: the pages of a filtered listing are that filtered listing once, got \
         {walked:?}"
    );

    // A job the clock has not finished is listed like any other, under whatever
    // non-terminal status it currently holds — and a filter naming only
    // statuses nothing here reaches admits it no more than it admits a finished
    // job.
    let running = emulator
        .start_ingestion_job(
            "the unfinished job",
            &knowledge_base_id,
            &running_source_id,
            json!({}),
        )
        .await;
    let case = "a job the clock has not finished";
    let (summaries, _) = emulator
        .page(
            case,
            &knowledge_base_id,
            &running_source_id,
            json!({ "filters": status_filter(&REACHABLE_STATUSES) }),
        )
        .await;
    assert_eq!(
        ids(case, &summaries),
        vec![running.clone()],
        "case {case}: the job is listed under one of the statuses a started job passes \
         through, got {summaries:?}"
    );
    let (refused_by_filter, _) = emulator
        .page(
            case,
            &knowledge_base_id,
            &running_source_id,
            json!({ "filters": status_filter(&UNREACHED_STATUSES) }),
        )
        .await;
    assert!(
        refused_by_filter.is_empty(),
        "case {case}: a filter naming only statuses nothing here reaches admits nothing, \
         got {refused_by_filter:?}"
    );
}

/// `sortBy` orders the listed set by the field and the direction it names: the
/// set is the same whichever way it is sorted, and the reported order is
/// monotone in the field asked for.
#[tokio::test]
async fn sort_by_orders_the_listed_jobs_by_the_field_and_direction_it_names() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the sorted store", "job-sort-kb")
        .await;
    let data_source_id = emulator
        .data_source("the sorted source", &knowledge_base_id, "job-sort-source")
        .await;

    let mut stored: Vec<String> = Vec::new();
    for position in 0..2 {
        stored.push(
            emulator
                .run_ingestion_job(
                    &format!("the finished job #{position}"),
                    &knowledge_base_id,
                    &data_source_id,
                    json!({}),
                )
                .await,
        );
    }
    // One job the clock has not carried to its terminal status yet, so the
    // listed jobs do not all share one status.
    stored.push(
        emulator
            .start_ingestion_job(
                "the unfinished job",
                &knowledge_base_id,
                &data_source_id,
                json!({}),
            )
            .await,
    );
    let mut as_stored_set = stored.clone();
    as_stored_set.sort();

    // Rows span both members of the modelled `IngestionJobSortByAttribute` in
    // both directions of `SortOrder`, plus the absent member
    // (`smithy_query(query="shape",shape="IngestionJobSortByAttribute,SortOrder")`).
    for (case, sort_by) in [
        ("no sortBy at all", None),
        (
            "startedAt ascending",
            Some(json!({ "attribute": "STARTED_AT", "order": "ASCENDING" })),
        ),
        (
            "startedAt descending",
            Some(json!({ "attribute": "STARTED_AT", "order": "DESCENDING" })),
        ),
        (
            "status ascending",
            Some(json!({ "attribute": "STATUS", "order": "ASCENDING" })),
        ),
        (
            "status descending",
            Some(json!({ "attribute": "STATUS", "order": "DESCENDING" })),
        ),
    ] {
        let mut body = json!({});
        if let Some(sort_by) = sort_by.clone() {
            body["sortBy"] = sort_by;
        }
        let (summaries, _) = emulator
            .page(case, &knowledge_base_id, &data_source_id, body)
            .await;
        let listed = ids(case, &summaries);
        let mut as_set = listed.clone();
        as_set.sort();
        assert_eq!(
            as_set, as_stored_set,
            "case {case}: sorting reports the same jobs in another order, got {listed:?}"
        );

        let descending = sort_by
            .as_ref()
            .and_then(|sort_by| sort_by["order"].as_str())
            == Some("DESCENDING");
        let on_status = sort_by
            .as_ref()
            .and_then(|sort_by| sort_by["attribute"].as_str())
            == Some("STATUS");
        // The sorted field of each reported summary, rendered so that comparing
        // the renderings compares the values.
        let keys: Vec<String> = summaries
            .iter()
            .map(|summary| {
                if on_status {
                    summary["status"].as_str().unwrap_or_default().to_string()
                } else {
                    instant_key(&summary["startedAt"])
                }
            })
            .collect();
        let monotone = keys.windows(2).all(|pair| {
            if descending {
                pair[0] >= pair[1]
            } else {
                pair[0] <= pair[1]
            }
        });
        assert!(
            monotone,
            "case {case}: the reported order must be monotone in the sorted field, got {keys:?}"
        );
    }
}

/// A knowledge base or data source the store does not hold — including a data
/// source a delete has already been accepted for — is refused as the missing
/// resource, and a `nextToken` this service never issued, a `maxResults`
/// outside the modelled range and a malformed `filters` are refused as bad
/// requests.
#[tokio::test]
async fn the_listing_refuses_an_unknown_or_deleted_resource_a_foreign_token_and_a_bad_request() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the refusing store", "job-refusal-kb")
        .await;
    let other_knowledge_base_id = emulator
        .knowledge_base("the other store", "job-refusal-other-kb")
        .await;
    let data_source_id = emulator
        .data_source("the refusing source", &knowledge_base_id, "job-refusal-source")
        .await;
    let foreign_data_source_id = emulator
        .data_source(
            "the other store's source",
            &other_knowledge_base_id,
            "job-refusal-foreign",
        )
        .await;
    let stored = vec![
        emulator
            .run_ingestion_job(
                "the stored job",
                &knowledge_base_id,
                &data_source_id,
                json!({}),
            )
            .await,
    ];

    // A knowledge base this store does not hold: the missing resource, not an
    // empty page. Rows span the forms a well-formed `Id` takes.
    for (case, unknown) in [
        ("an id nothing was minted for", "aZ09aZ09aZ"),
        ("an all-digit id", "0123456789"),
        ("an all-letter id", "abcdefghij"),
    ] {
        let refused = emulator.list(unknown, &data_source_id, json!({})).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({unknown}): expected ListIngestionJobs -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({unknown}): expected the modelled ResourceNotFoundException, got \
             {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!("KnowledgeBase with id {unknown} is not found."),
            "case {case} ({unknown}): expected the missing knowledge base to be worded with \
             the id the request carried, got {refused:?}"
        );
    }

    // A data source the addressed knowledge base does not hold: an id nothing
    // was minted for, another knowledge base's data source, and one a delete
    // has already been accepted for are all equally absent here — and each is
    // refused as the DATA SOURCE the request named.
    let deleted_data_source_id = emulator
        .data_source("the deleted source", &knowledge_base_id, "job-refusal-deleted")
        .await;
    let accepted = emulator
        .delete_data_source(&knowledge_base_id, &deleted_data_source_id)
        .await;
    assert_eq!(
        accepted.status, 202,
        "case setup: expected DeleteDataSource -> 202, got {accepted:?}"
    );
    let removed_data_source_id = emulator
        .data_source("the removed source", &knowledge_base_id, "job-refusal-removed")
        .await;
    let accepted = emulator
        .delete_data_source(&knowledge_base_id, &removed_data_source_id)
        .await;
    assert_eq!(
        accepted.status, 202,
        "case setup: expected DeleteDataSource -> 202, got {accepted:?}"
    );
    emulator
        .poll_data_source_until_gone(&knowledge_base_id, &removed_data_source_id)
        .await;

    for (case, absent) in [
        ("an id nothing was minted for", "aZ09aZ09aZ".to_string()),
        ("an all-digit id", "0123456789".to_string()),
        (
            "another knowledge base's data source",
            foreign_data_source_id.clone(),
        ),
        (
            "a data source whose delete was accepted",
            deleted_data_source_id.clone(),
        ),
        (
            "a data source the clock has already removed",
            removed_data_source_id.clone(),
        ),
    ] {
        let refused = emulator.list(&knowledge_base_id, &absent, json!({})).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({absent}): expected ListIngestionJobs -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({absent}): expected the modelled ResourceNotFoundException, got \
             {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!("DataSource with id {absent} is not found."),
            "case {case} ({absent}): expected the missing data source to be worded with the \
             id the request carried, got {refused:?}"
        );
    }

    // Rows: strings no page of this listing handed out. A position is a count
    // of what has been reported, so nothing else names one.
    for (case, token) in [
        ("a token from somewhere else", "not-a-pagination-token"),
        ("a bare identifier", "aZ09aZ09aZ"),
        ("a position of nothing reported", "0"),
        ("a position that is not a whole number", "1.5"),
        ("a hexadecimal position", "0x10"),
        ("a signed position", "+1"),
        ("a zero-padded position", "01"),
        ("another listing's composite position", "1700000000000000000-aZ09aZ09aZ"),
    ] {
        let refused = emulator
            .list(
                &knowledge_base_id,
                &data_source_id,
                json!({ "nextToken": token }),
            )
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({token}): expected ListIngestionJobs -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({token}): expected the modelled ValidationException, got {:?}",
            refused.error_type
        );
    }

    // `maxResults` is modelled `range 1..1000`, `nextToken` `length 1..2048,
    // pattern ^\S*$`, `filters` `length 1..1` over a filter whose members are
    // single-valued enums: one past a published bound, and a value no modelled
    // enum declares, are refused before the handler runs.
    let long_token = "a".repeat(2049);
    let two_filters = json!([
        { "attribute": "STATUS", "operator": "EQ", "values": ["COMPLETE"] },
        { "attribute": "STATUS", "operator": "EQ", "values": ["STARTING"] },
    ]);
    for (case, body) in [
        (
            "one below the maxResults lower bound",
            json!({ "maxResults": 0 }),
        ),
        (
            "one above the maxResults upper bound",
            json!({ "maxResults": 1001 }),
        ),
        ("a negative maxResults", json!({ "maxResults": -1 })),
        ("an empty nextToken", json!({ "nextToken": "" })),
        ("a nextToken carrying whitespace", json!({ "nextToken": "1 2" })),
        (
            "a nextToken past its length bound",
            json!({ "nextToken": long_token }),
        ),
        ("an empty filters list", json!({ "filters": [] })),
        ("one filter past the filters bound", json!({ "filters": two_filters })),
        (
            "a filter attribute no enum declares",
            json!({
                "filters": [{ "attribute": "STARTED_AT", "operator": "EQ", "values": ["x"] }]
            }),
        ),
        (
            "a filter operator no enum declares",
            json!({
                "filters": [{ "attribute": "STATUS", "operator": "NE", "values": ["x"] }]
            }),
        ),
        (
            "a sortBy attribute no enum declares",
            json!({ "sortBy": { "attribute": "UPDATED_AT", "order": "ASCENDING" } }),
        ),
        (
            "a sortBy order no enum declares",
            json!({ "sortBy": { "attribute": "STATUS", "order": "DOWN" } }),
        ),
    ] {
        let refused = emulator
            .list(&knowledge_base_id, &data_source_id, body.clone())
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({body}): expected ListIngestionJobs -> 400, got {refused:?}"
        );
    }

    // Every refusal above left the stored jobs exactly as they were.
    let walked = walk(
        &emulator,
        "after the refusals",
        &knowledge_base_id,
        &data_source_id,
        &json!({}),
        None,
    )
    .await;
    assert_eq!(
        walked, stored,
        "case after the refusals: a refused listing changes nothing, got {walked:?}"
    );
}

/// Every timestamp an ingestion job reports is on the wire in the modelled
/// `date-time` form — an RFC-3339 string, never the epoch number the restJson1
/// default writes for a timestamp whose format is dropped.
///
/// `IngestionJob.startedAt`/`updatedAt`/`textReadyAt` and the same three members
/// of `IngestionJobSummary` all target the one `DateTimestamp` shape, which
/// carries `@timestampFormat("date-time")`
/// (`smithy_query(query="shape",shape="IngestionJob,IngestionJobSummary")`), so
/// the rule is one rule across the start, the read and the listing, which report
/// the same instants in that form.
#[tokio::test]
async fn every_ingestion_job_timestamp_is_reported_in_the_modelled_date_time_wire_form() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the timestamped store", "job-wire-kb")
        .await;
    let data_source_id = emulator
        .data_source("the timestamped source", &knowledge_base_id, "job_wire_src")
        .await;

    // A job the clock has carried to COMPLETE carries all three modelled
    // timestamps; one still running carries the two a running job has and no
    // `textReadyAt`. The rows below span both.
    let complete = emulator
        .run_ingestion_job(
            "the completed job",
            &knowledge_base_id,
            &data_source_id,
            json!({}),
        )
        .await;

    let started = emulator
        .call(
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/"
            ),
            Some(json!({ "description": "a job the clock has not finished" })),
        )
        .await;
    assert_eq!(
        started.status, 202,
        "case the started job: expected StartIngestionJob -> 202, got {started:?}"
    );
    let running = started.string(&["ingestionJob", "ingestionJobId"]);
    // StartIngestionJob's own answer: the two instants a job has at its start.
    let start_started_at = crate::update_round_trip::date_time_wire_instant(
        "the started job",
        "ingestionJob.startedAt",
        started.member(&["ingestionJob", "startedAt"]),
    );
    let start_updated_at = crate::update_round_trip::date_time_wire_instant(
        "the started job",
        "ingestionJob.updatedAt",
        started.member(&["ingestionJob", "updatedAt"]),
    );
    assert!(
        start_updated_at >= start_started_at,
        "case the started job: expected updatedAt at or after startedAt \
         {start_started_at}, got {start_updated_at}"
    );
    assert!(
        started.member(&["ingestionJob", "textReadyAt"]).is_null(),
        "case the started job: no text is queryable before the job completes, got {:?}",
        started.member(&["ingestionJob", "textReadyAt"])
    );

    for (case, ingestion_job_id, text_is_ready) in [
        ("a completed job", complete.as_str(), true),
        ("a running job", running.as_str(), false),
    ] {
        let read = emulator
            .get_ingestion_job(&knowledge_base_id, &data_source_id, ingestion_job_id)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetIngestionJob -> 200, got {read:?}"
        );
        let started_at = crate::update_round_trip::date_time_wire_instant(
            case,
            "ingestionJob.startedAt",
            read.member(&["ingestionJob", "startedAt"]),
        );
        let updated_at = crate::update_round_trip::date_time_wire_instant(
            case,
            "ingestionJob.updatedAt",
            read.member(&["ingestionJob", "updatedAt"]),
        );
        assert!(
            updated_at >= started_at,
            "case {case}: expected updatedAt at or after startedAt {started_at}, \
             got {updated_at}"
        );
        if text_is_ready {
            let text_ready_at = crate::update_round_trip::date_time_wire_instant(
                case,
                "ingestionJob.textReadyAt",
                read.member(&["ingestionJob", "textReadyAt"]),
            );
            assert!(
                text_ready_at >= started_at,
                "case {case}: expected textReadyAt at or after startedAt {started_at}, \
                 got {text_ready_at}"
            );
        } else {
            assert!(
                read.member(&["ingestionJob", "textReadyAt"]).is_null(),
                "case {case}: no text is queryable before the job completes, got {:?}",
                read.member(&["ingestionJob", "textReadyAt"])
            );
        }

        // The listing reports the same instants, in the same form.
        let (summaries, _) = emulator
            .page(case, &knowledge_base_id, &data_source_id, json!({}))
            .await;
        let summary = summaries
            .iter()
            .find(|summary| summary_id(case, summary) == ingestion_job_id)
            .unwrap_or_else(|| {
                panic!("case {case}: the listing omits {ingestion_job_id}: {summaries:?}")
            });
        for member in ["startedAt", "updatedAt"] {
            crate::update_round_trip::assert_date_time_wire_form(case, member, &summary[member]);
            assert_eq!(
                &summary[member],
                read.member(&["ingestionJob", member]),
                "case {case}: expected the summary to report the stored {member}, got {:?}",
                summary[member]
            );
        }
        assert_eq!(
            &summary["textReadyAt"],
            read.member(&["ingestionJob", "textReadyAt"]),
            "case {case}: expected the summary to report the stored textReadyAt, got {:?}",
            summary["textReadyAt"]
        );
    }
}

