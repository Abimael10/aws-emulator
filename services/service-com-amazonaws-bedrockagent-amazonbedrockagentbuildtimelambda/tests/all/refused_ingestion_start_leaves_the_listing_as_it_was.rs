// SPDX-License-Identifier: CC0-1.0

//! What `ListIngestionJobs` ENUMERATES after a start the service refused: the
//! jobs it accepted for that knowledge base / data source pair, and nothing
//! else.
//!
//! The rule this suite pins — the pre-condition that decides WHICH starts are
//! accepted is pinned next door in `data_source_preconditions`, so the rows
//! here differ along what the listing already holds rather than along the
//! refusal:
//!
//! * a refused start persists no ingestion job, so the @required
//!   `ingestionJobSummaries` of a pair no start was ever accepted for is
//!   answered PRESENT and EMPTY rather than omitted or carrying a phantom job;
//! * a pair that already holds an accepted job keeps enumerating exactly that
//!   job, by the `ingestionJobId` the acceptance minted, across a later refusal;
//! * the sentence the refusal words takes its one variable part from the data
//!   source's CURRENT status, as a read of that data source reports it.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly, and every wait is a poll for the CONDITION the
//! clock owes rather than a fixed sleep.

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

/// The fixed text of the `ConflictException` sentence a start refused on a data
/// source whose status does not serve ingestion words; the data source's own
/// `DataSourceStatus` and a period close it.
const BLOCKED_PREFIX: &str = "You cannot start an ingestion job on a data source with status ";

/// The one `DataSourceStatus` the model documents as ready for ingestion, which
/// a refusal therefore never names.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceStatus")`).
const READY_FOR_INGESTION: &str = "AVAILABLE";

/// The `DataSourceStatus` values the model declares for a data source the
/// service is still working on — the ones an accepted create and an accepted
/// update leave it in while that change is applied — so a status outside them
/// and outside [`READY_FOR_INGESTION`] is one the clock owes nothing more
/// after.
///
/// Source: as [`READY_FOR_INGESTION`].
const TRANSIENT_STATUSES: [&str; 2] = ["CREATING", "UPDATING"];

/// How long a poll waits for a status the CLOCK owes: many times the spans this
/// service owes, so a loaded machine still converges, and bounded so a hung poll
/// fails the test rather than hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before reading again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The `x-amzn-errortype` header, which a restJson1 client reads before the
    /// body; `None` on a success.
    error_type_header: Option<String>,
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
        let error_type_header = response
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
        Answer {
            status,
            error_type_header,
            body,
        }
    }

    /// Create the service-managed knowledge base the rows here attach their data
    /// sources to, and report its minted id.
    async fn knowledge_base(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "MANAGED",
                        "managedKnowledgeBaseConfiguration": { "embeddingModelType": "MANAGED" },
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

    /// Attach one data source to a knowledge base and report its minted id.
    async fn data_source(&self, case: &str, knowledge_base_id: &str, request: Value) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(request),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected CreateDataSource -> 200, got {created:?}"
        );
        created.string(&["dataSource", "dataSourceId"])
    }

    async fn update_data_source(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) {
        let updated = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
                Some(request),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected UpdateDataSource -> 200, got {updated:?}"
        );
    }

    /// The status a read of the data source reports for it.
    async fn data_source_status(&self, knowledge_base_id: &str, data_source_id: &str) -> String {
        let read = self
            .call(
                "GET",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
                None,
            )
            .await;
        assert_eq!(
            read.status, 200,
            "expected GetDataSource -> 200, got {read:?}"
        );
        read.string(&["dataSource", "status"])
    }

    /// Read the data source until it has SETTLED on a status that does not
    /// serve ingestion — neither the one that does nor one of the transient
    /// ones the service is still working through — and report that status. The
    /// CONDITION is what is waited for, not a span of time, and a settled
    /// status is what makes the sentence of the next refusal the data source's
    /// own rather than a race.
    async fn data_source_settled_outside_ready(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
    ) -> String {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let reported = self
                .data_source_status(knowledge_base_id, data_source_id)
                .await;
            if reported != READY_FOR_INGESTION && !TRANSIENT_STATUSES.contains(&reported.as_str())
            {
                return reported;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: expected the data source to settle on a status outside \
                 {READY_FOR_INGESTION} and {TRANSIENT_STATUSES:?}, it still reports \
                 {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `StartIngestionJob`, exactly as a client sends it — whatever the service
    /// answers.
    async fn start(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/"
            ),
            Some(json!({})),
        )
        .await
    }

    /// The ids `ListIngestionJobs` enumerates for this pair, in the order it
    /// enumerates them: the whole listing, asserted to be the modelled success
    /// whose @required list member is PRESENT — an omitted member fails here,
    /// where counting its entries would not — and to fit one page.
    async fn listed_job_ids(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
    ) -> Vec<String> {
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
        let summaries = listed.body["ingestionJobSummaries"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected the required ingestionJobSummaries to be present as \
                     a list, got {listed:?}"
                )
            })
            .clone();
        assert!(
            listed.body["nextToken"].is_null(),
            "case {case}: expected the whole listing in one page and so no nextToken, \
             got {listed:?}"
        );
        summaries
            .iter()
            .map(|summary| {
                summary["ingestionJobId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case}: a summary carries no ingestionJobId: {summary:?}")
                    })
                    .to_string()
            })
            .collect()
    }
}

/// A `MANAGED_KNOWLEDGE_BASE_CONNECTOR` data source whose connector names NO
/// source outside the service — its documents are pushed in through the
/// service's own document API — so it is ready for ingestion as soon as it is
/// stored.
///
/// Source: the `DataSourceConfiguration` and
/// `ManagedKnowledgeBaseConnectorConfiguration` structures of this service's
/// Smithy model (`smithy_query(query="shape",shape="DataSourceConfiguration")`).
fn connector_over_no_external_source(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceConfiguration": {
            "type": "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
            "managedKnowledgeBaseConnectorConfiguration": {
                "connectorParameters": { "type": "CUSTOM", "version": "1" },
            },
        },
    })
}

/// A `MANAGED_KNOWLEDGE_BASE_CONNECTOR` data source whose connector names a
/// source OUTSIDE the service: that connection has to be brought up before the
/// data source can serve ingestion, so it does not serve ingestion once the
/// clock has settled the outcome of bringing it up.
///
/// Source: as [`connector_over_no_external_source`].
fn connector_over_an_external_source(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceConfiguration": {
            "type": "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
            "managedKnowledgeBaseConnectorConfiguration": {
                "connectorParameters": {
                    "type": "WEB",
                    "version": "1",
                    "connectionConfiguration": { "url": "https://example.com/docs" },
                },
            },
        },
    })
}

/// Assert `answer` is the modelled conflict a start on a data source in
/// `status` is refused with: the status the data source itself reports is what
/// closes the sentence, so nothing here is a value of this test's own.
fn assert_blocked_by_status(case: &str, status: &str, answer: &Answer) {
    assert_ne!(
        status, READY_FOR_INGESTION,
        "case {case}: this row needs a data source that does not serve ingestion"
    );
    assert_eq!(
        answer.status, 409,
        "case {case}: expected HTTP 409, got {answer:?}"
    );
    assert_eq!(
        answer.error_type_header.as_deref(),
        Some("ConflictException"),
        "case {case}: expected x-amzn-errortype: ConflictException, got {answer:?}"
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(format!("{BLOCKED_PREFIX}{status}.").as_str()),
        "case {case}: expected the sentence {BLOCKED_PREFIX:?} closed by the status the data \
         source reports ({status}) and a period, got {answer:?}"
    );
}

/// `ListIngestionJobs` enumerates exactly the starts the service accepted: the
/// @required list is answered present and EMPTY for a pair whose only start was
/// refused, and a pair that already holds an accepted job keeps enumerating
/// exactly that job across a later refusal.
#[tokio::test]
async fn the_listing_enumerates_only_the_starts_the_service_accepted() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base("the gated store", "refused-start-kb")
        .await;

    // Row 1 — nothing accepted for this pair: a data source whose connector has
    // a source outside the service to reach does not serve ingestion once the
    // clock has settled the outcome of bringing that connection up, so the one
    // start issued for it is refused and the listing stays present and empty.
    let case = "a data source that does not serve ingestion, no start accepted";
    let external = emulator
        .data_source(
            case,
            &knowledge_base_id,
            connector_over_an_external_source("refused-start-external"),
        )
        .await;
    let status = emulator
        .data_source_settled_outside_ready(case, &knowledge_base_id, &external)
        .await;
    assert_eq!(
        emulator
            .listed_job_ids(case, &knowledge_base_id, &external)
            .await,
        Vec::<String>::new(),
        "case {case}: a pair nothing was started for enumerates nothing"
    );
    let refused = emulator.start(&knowledge_base_id, &external).await;
    assert_blocked_by_status(case, &status, &refused);
    assert_eq!(
        emulator
            .listed_job_ids(case, &knowledge_base_id, &external)
            .await,
        Vec::<String>::new(),
        "case {case}: the refused start persisted no job, so the listing is still empty"
    );

    // Row 2 — one accepted start already in the listing: the job that
    // acceptance minted is what the listing reports, by the id it answered
    // with.
    let case = "a data source ready for ingestion";
    let ready = emulator
        .data_source(
            case,
            &knowledge_base_id,
            connector_over_no_external_source("refused-start-ready"),
        )
        .await;
    let started = emulator.start(&knowledge_base_id, &ready).await;
    assert_eq!(
        started.status, 202,
        "case {case}: expected StartIngestionJob -> 202, got {started:?}"
    );
    let accepted = started.string(&["ingestionJob", "ingestionJobId"]);
    assert_eq!(
        emulator
            .listed_job_ids(case, &knowledge_base_id, &ready)
            .await,
        vec![accepted.clone()],
        "case {case}: the accepted start is the one job the listing enumerates"
    );

    // An update that stores a configuration whose connection has to be brought
    // up moves this data source out of the status that serves ingestion, so the
    // next start is refused — and the listing still reports exactly the job the
    // earlier acceptance minted.
    let case = "a data source an update moved out of the status that serves ingestion, \
                one start accepted";
    emulator
        .update_data_source(
            case,
            &knowledge_base_id,
            &ready,
            connector_over_an_external_source("refused-start-ready"),
        )
        .await;
    let status = emulator
        .data_source_settled_outside_ready(case, &knowledge_base_id, &ready)
        .await;
    let refused = emulator.start(&knowledge_base_id, &ready).await;
    assert_blocked_by_status(case, &status, &refused);
    assert_eq!(
        emulator
            .listed_job_ids(case, &knowledge_base_id, &ready)
            .await,
        vec![accepted],
        "case {case}: the refused start persisted no job, so the listing still enumerates \
         only the accepted one"
    );
}
