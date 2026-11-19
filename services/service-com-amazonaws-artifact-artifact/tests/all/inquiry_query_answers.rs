// SPDX-License-Identifier: CC0-1.0

//! The ANSWER one compliance-inquiry query carries, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rules under test are the ones the answer plane states:
//!
//! * a query is answered out of the published report catalog this service serves: the
//!   `response` a query reports is the passages of the catalog documents whose text carries
//!   the query's terms, each under the source it was read out of, and the `citations`
//!   beside it name those sources, quote those same passages and link to where this
//!   emulator publishes them;
//! * a query the catalog holds nothing bearing on — and one naming no term at all —
//!   reports NEITHER member: there is no passage to quote, so none is invented;
//! * the answer is released by the CLOCK and by no read: a query whose processing is still
//!   in progress reports neither member, every listing after the due instant reports the
//!   same answer, and reading it twice changes nothing;
//! * a citation's link resolves back to this emulator — it LOCATES the cited report's own
//!   read, at the version the passage was quoted from — while the document behind that
//!   report stays reachable only the way every other caller reaches it, by accepting the
//!   term governing it.
//!
//! Two further rules about the RECORD the query history serves are pinned at the end of
//! this suite:
//!
//! * the history serves ONE `QuerySummary` per query the inquiry holds, in the service's
//!   own order (ascending `queryIdentifier` from 1), with every member the model requires
//!   populated — and the whole collection is enumerable whatever page size a caller walks
//!   it at, so no page under-reports it;
//! * a citation is ONE consistent triple: its `sourceLink` locates exactly the source its
//!   `sourceLabel` names and its `sourceContent` quotes, and every projection of that
//!   citation — the query listing and the exported document — reports the same triple.
//!
//! The rows differ along what those rules generalise over: the TERMS the query names (a
//! series, a report name, prose from a published description, one term, several, none at
//! all, and terms no catalog document carries) and the FORM of the submission (a text
//! query, and an uploaded document whose section labels carry the terms), as well as how
//! many queries one inquiry holds (one, two and three) and the page size the history is
//! walked at.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_artifact_artifact::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The endpoint the test addresses the emulator on, which every URL this service announces
/// has to name for the announced URL to resolve.
const HOST: &str = "artifact.eu-west-1.amazonaws.com";

/// A SigV4 credential scope naming this service, which is what routes the request.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/artifact/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The span after which an inquiry's processing is due to be over, plus slack. The handler
/// stores the due instant rather than sleeping, so only the test waits.
///
/// Must stay above `crate::state::INQUIRY_PROCESSING_SPAN` (500 ms at the time of
/// writing): if that span is ever raised, raise this with it.
///
/// `std::thread::sleep` rather than `tokio::time::sleep`: the workspace pins tokio without
/// its `time` feature.
const PAST_THE_PROCESSING_SPAN: Duration = Duration::from_millis(900);

/// The most sources one answer cites — the bound the answer plane states
/// (`crate::state::ANSWER_CITATION_CAPACITY`), spelled out here so a raised bound fails
/// this suite instead of redefining what it asserts.
const CITATION_CAPACITY: usize = 3;

/// `LongStringAttribute`'s published maximum (`@length(min: 1, max: 2048)`), which every
/// member of the answer targets.
const LONG_STRING_MAX: usize = 2048;

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Bytes,
    signed: bool,
) -> Answer {
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
        .header("content-type", "application/json");
    if signed {
        request = request.header("authorization", SIGNED);
    }
    let request = request
        .body(body)
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer { status, body }
}

/// Create one inquiry and return the `InquiryId` it minted.
async fn create(router: &MultiServiceRouter, body: &Value) -> String {
    let answer = send(
        router,
        "POST",
        "/v1/compliance-inquiry/create",
        Bytes::from(body.to_string()),
        true,
    )
    .await;
    assert_eq!(
        answer.status, 202,
        "the create must be accepted, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["complianceInquirySummary"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the create must report an inquiry id, got {:?}", answer.body))
        .to_string()
}

/// The queries one inquiry reports.
async fn list_queries(router: &MultiServiceRouter, inquiry_id: &str) -> Vec<Value> {
    let answer = send(
        router,
        "GET",
        &format!("/v1/compliance-inquiry/listQueries?complianceInquiryId={inquiry_id}"),
        Bytes::new(),
        true,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "listing an inquiry's queries must succeed, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["queries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The catalog as the API reports it, so what this suite checks a citation against is
/// whatever this emulator publishes rather than a list written out here.
async fn catalog(router: &MultiServiceRouter) -> Vec<Value> {
    let answer = send(router, "GET", "/v1/report/list", Bytes::new(), true).await;
    let reports = answer.body["reports"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !reports.is_empty(),
        "the catalog must publish reports for a query to be answered out of, got {:?}",
        answer.body
    );
    reports
}

/// A `CreateComplianceInquiry` body for a single submitted query.
fn text_body(name: &str, query: &str) -> Value {
    json!({ "name": name, "inquiryContent": { "query": query } })
}

/// A `CreateComplianceInquiry` body for an uploaded document processed section by section.
fn file_body(name: &str, sections: &[&str]) -> Value {
    json!({
        "name": name,
        "inquiryContent": { "fileContent": { "content": "cXVlc3Rpb25uYWlyZQ==", "fileSections": sections } },
    })
}

/// One submission, and what the catalog must have been found to hold for each of the
/// queries it submits.
struct Case {
    label: &'static str,
    body: Value,
    /// Per query, in `queryIdentifier` order: the text a cited source must carry when the
    /// catalog holds one bearing on that query, and `None` when it holds none at all.
    ///
    /// The expected text is a term of the query the catalog really publishes — the series,
    /// name or published description of a report — so the row says WHICH source answers it
    /// without restating the ranking.
    expected: Vec<Option<&'static str>>,
}

/// The submissions this suite covers.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a query naming a report series and its standard",
            body: text_body("Certification", "Which ISO 27001 certification applies?"),
            expected: vec![Some("ISO 27001")],
        },
        Case {
            label: "a query naming prose from a published description",
            body: text_body(
                "Card data",
                "Who assessed the cardholder data environment?",
            ),
            expected: vec![Some("cardholder")],
        },
        Case {
            label: "a query naming one term",
            body: text_body("Outposts", "Outposts"),
            expected: vec![Some("Outposts")],
        },
        Case {
            label: "a query naming terms no catalog document carries",
            body: text_body(
                "Unknown",
                "zzqqxx wibblewobble nonexistentcomplianceterm",
            ),
            expected: vec![None],
        },
        Case {
            label: "a query naming no term at all",
            body: text_body("Punctuation", "?!"),
            expected: vec![None],
        },
        Case {
            label: "an uploaded document whose sections name a series and nothing",
            body: file_body(
                "Vendor questionnaire",
                &["SOC 2 Type 2 controls", "zzqqxx wibblewobble"],
            ),
            expected: vec![Some("SOC"), None],
        },
    ]
}

/// The report the catalog publishes under `name`, as the listing reports it.
fn published<'reports>(reports: &'reports [Value], name: &str) -> &'reports Value {
    reports
        .iter()
        .find(|report| report["name"].as_str() == Some(name))
        .unwrap_or_else(|| {
            panic!("a cited source must be a report the catalog publishes, got {name:?}")
        })
}

/// The path and query of a URL this service announced, as a caller fetches it.
fn fetchable(url: &str) -> String {
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("an announced URL must carry a scheme, got {url:?}"));
    let (host, path) = after_scheme
        .split_once('/')
        .unwrap_or_else(|| panic!("an announced URL must carry a path, got {url:?}"));
    assert_eq!(
        host, HOST,
        "a citation must link to this emulator so it resolves, got {url:?}"
    );
    format!("/{path}")
}

#[tokio::test]
async fn an_answer_quotes_the_catalog_sources_the_query_matched() {
    let router = emulator();
    let reports = catalog(&router).await;

    for case in cases() {
        let label = case.label;
        let id = create(&router, &case.body).await;
        // The answer is released at the inquiry's due instant, so the listing that reads it
        // is taken after that span.
        std::thread::sleep(PAST_THE_PROCESSING_SPAN);
        let queries = list_queries(&router, &id).await;
        assert_eq!(
            queries.len(),
            case.expected.len(),
            "case {label:?}: expected {} queries, got {queries:?}",
            case.expected.len()
        );

        for (position, expected) in case.expected.iter().enumerate() {
            let query = &queries[position];
            let response = query["response"].as_str();
            let citations = query["citations"].as_array();

            let Some(expected_source_text) = expected else {
                assert!(
                    response.is_none() && citations.is_none(),
                    "case {label:?}: query {position} has no source in the catalog, so it \
                     reports neither a response nor a citation, got {query:?}"
                );
                continue;
            };

            let response = response.unwrap_or_else(|| {
                panic!(
                    "case {label:?}: query {position} must report the answer retrieved for \
                     it, got {query:?}"
                )
            });
            let citations = citations.cloned().unwrap_or_default();
            assert!(
                !citations.is_empty() && citations.len() <= CITATION_CAPACITY,
                "case {label:?}: query {position} must cite between one and \
                 {CITATION_CAPACITY} sources, got {citations:?}"
            );
            assert!(
                response.chars().count() <= LONG_STRING_MAX
                    && !response.contains(['<', '>'])
                    && !response.is_empty(),
                "case {label:?}: query {position} must report a response inside \
                 LongStringAttribute's bounds, got {response:?}"
            );

            // The response IS the cited passages, in citation order: each source's label
            // and the passage quoted under it are in the text, and the later citation's
            // passage comes after the earlier one's.
            let mut quoted_at = 0usize;
            let mut labels: Vec<&str> = Vec::new();
            for citation in &citations {
                let source_label = citation["sourceLabel"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must label its source, got {citation:?}")
                });
                let source_content = citation["sourceContent"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must quote its source, got {citation:?}")
                });
                let source_link = citation["sourceLink"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must link to its source, got {citation:?}")
                });
                labels.push(source_label);

                // The source is a report this catalog really publishes, and the passage is
                // text that report really carries.
                let report = published(&reports, source_label);
                let description = report["description"].as_str().unwrap_or_default();
                let name = report["name"].as_str().unwrap_or_default();
                assert!(
                    description.contains(source_content) || name.contains(source_content),
                    "case {label:?}: a cited passage must be text the source publishes, \
                     got {source_content:?} for {source_label:?}"
                );
                let report_id = report["id"].as_str().unwrap_or_default();
                assert!(
                    source_link.contains(report_id),
                    "case {label:?}: a citation must link to the document it quoted, got \
                     {source_link:?} for {report_id:?}"
                );

                let at = response.find(source_content).unwrap_or_else(|| {
                    panic!(
                        "case {label:?}: the response is the cited passages, so it must \
                         carry {source_content:?}, got {response:?}"
                    )
                });
                assert!(
                    at >= quoted_at,
                    "case {label:?}: the response states its sources in citation order, \
                     got {source_content:?} at {at} after {quoted_at}"
                );
                quoted_at = at;
                assert!(
                    response.contains(source_label),
                    "case {label:?}: a quoted passage is stated under the source it was \
                     read out of, got {response:?} without {source_label:?}"
                );
            }

            // The sources are the ones the query's own terms were found in, which is what
            // the row names.
            assert!(
                citations.iter().any(|citation| {
                    let label = citation["sourceLabel"].as_str().unwrap_or_default();
                    let content = citation["sourceContent"].as_str().unwrap_or_default();
                    label.contains(expected_source_text) || content.contains(expected_source_text)
                }),
                "case {label:?}: query {position} must be answered out of the source \
                 carrying {expected_source_text:?}, got {citations:?}"
            );
            labels.sort_unstable();
            let distinct = labels.len();
            labels.dedup();
            assert_eq!(
                labels.len(),
                distinct,
                "case {label:?}: each source is cited once, got {citations:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_answer_is_released_by_the_clock_and_then_never_changes() {
    let router = emulator();
    let id = create(
        &router,
        &text_body("Certification", "Which ISO 27001 certification applies?"),
    )
    .await;

    // Inside the processing span the query reports no answer: the response is released
    // when the processing that produced it is over, not when somebody looks. The handler
    // stores the due instant rather than sleeping, so the read that follows the create is
    // inside the span.
    let inside = list_queries(&router, &id).await;
    let query = &inside[0];
    assert_eq!(
        query["status"].as_str(),
        Some("PROCESSING"),
        "a query read inside the inquiry's processing span is still in progress, got {query:?}"
    );
    assert!(
        query["response"].is_null() && query["citations"].is_null(),
        "a query whose processing is still in progress reports no answer, got {query:?}"
    );

    std::thread::sleep(PAST_THE_PROCESSING_SPAN);
    let first = list_queries(&router, &id).await;
    assert!(
        first[0]["response"].as_str().is_some(),
        "past the processing span the query must report its answer, got {:?}",
        first[0]
    );

    // A read changes nothing a later caller sees.
    let second = list_queries(&router, &id).await;
    assert_eq!(
        (first[0]["response"].clone(), first[0]["citations"].clone()),
        (second[0]["response"].clone(), second[0]["citations"].clone()),
        "two listings must report the same answer, got {:?} then {:?}",
        first[0],
        second[0]
    );
}

#[tokio::test]
async fn a_citation_links_to_the_source_its_passage_was_read_from() {
    let router = emulator();
    let reports = catalog(&router).await;
    let id = create(
        &router,
        &text_body("Audit", "Which independent auditor assessed the AWS controls?"),
    )
    .await;
    std::thread::sleep(PAST_THE_PROCESSING_SPAN);
    let queries = list_queries(&router, &id).await;
    let citations = queries[0]["citations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !citations.is_empty(),
        "the query must cite the sources it was answered out of, got {:?}",
        queries[0]
    );

    for citation in &citations {
        let source_label = citation["sourceLabel"].as_str().unwrap_or_default();
        let source_link = citation["sourceLink"].as_str().unwrap_or_default();
        let report = published(&reports, source_label);
        // A citation LOCATES its source: following the link reads the cited report itself,
        // at the version the passage was quoted from. The document behind that report is
        // reached the way every other caller reaches it — by accepting its term — so the
        // link stands in for no acceptance and leads nowhere this emulator refuses to
        // serve.
        let followed = send(&router, "GET", &fetchable(source_link), Bytes::new(), true).await;
        assert_eq!(
            followed.status, 200,
            "case {source_label:?}: a citation's link must resolve back to this emulator, \
             got {} with {:?}",
            followed.status, followed.body
        );
        assert_eq!(
            (
                followed.body["reportDetails"]["id"].clone(),
                followed.body["reportDetails"]["name"].clone(),
                followed.body["reportDetails"]["version"].clone(),
            ),
            (
                report["id"].clone(),
                report["name"].clone(),
                report["version"].clone(),
            ),
            "case {source_label:?}: the link must read the cited report at the version the \
             passage came from, got {:?}",
            followed.body
        );
    }
}

/// How long a poll loop waits for the transition the CLOCK owes an inquiry before giving
/// up. Deliberately generous: the span itself is not what any case below is about, and a
/// loaded host reaches the due instant later than an idle one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one turn of a poll loop rests before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `QueryStatus`'s values — the closed enum `QuerySummary$status` reports
/// (`smithy_query(query="shape",shape="QueryStatus")`).
const QUERY_STATUSES: &[&str] = &["PROCESSING", "COMPLETED", "FAILED"];

/// `QueryStatusMessage`'s values — the closed enum `QuerySummary$statusMessage` reports
/// (`smithy_query(query="shape",shape="QueryStatusMessage")`).
const QUERY_STATUS_MESSAGES: &[&str] = &[
    "Query processing is complete.",
    "Query processing is in-progress.",
    "An internal error occurred while processing the query. Try again at a later time.",
    "Query is pending human review.",
    "Query contains restricted or unsupported content.",
];

/// The most queries one submission can name, so a page walk below is bounded: an uploaded
/// document names at most 30 sections (`InquiryFileContent$fileSections` is
/// `@length(min: 0, max: 30)`) and a text submission is one query.
const QUERIES_PER_INQUIRY_CEILING: usize = 30;

/// The queries one inquiry holds once the clock has released their answers.
///
/// Polled for the CONDITION — no query left in `PROCESSING` — rather than waited out, so
/// the suite asserts on the transition and not on how fast the host runs.
async fn settled_queries(router: &MultiServiceRouter, case: &str, inquiry_id: &str) -> Vec<Value> {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let queries = list_queries(router, inquiry_id).await;
        let settled = !queries.is_empty()
            && queries
                .iter()
                .all(|query| query["status"].as_str() != Some("PROCESSING"));
        if settled {
            return queries;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the inquiry's queries must leave PROCESSING once their \
             processing is due, got {queries:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Every query ONE whole walk of the history reported at `page_size`, in the order the
/// pages reported them.
///
/// Asserts only what makes the walk a walk — every page is served, a page holds at most
/// `maxResults` records, and the walk terminates — so each case's own rule is asserted
/// over what this hands back.
async fn walk_queries(
    router: &MultiServiceRouter,
    case: &str,
    inquiry_id: &str,
    page_size: i32,
) -> Vec<Value> {
    let mut walked: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut page = 0usize;
    loop {
        page += 1;
        assert!(
            page <= QUERIES_PER_INQUIRY_CEILING + 1,
            "case {case:?}: a walk at maxResults={page_size} must terminate, got {page} pages"
        );
        let mut uri = format!(
            "/v1/compliance-inquiry/listQueries?complianceInquiryId={inquiry_id}\
             &maxResults={page_size}"
        );
        if let Some(value) = token.as_deref() {
            uri.push_str(&format!("&nextToken={value}"));
        }
        let answer = send(router, "GET", &uri, Bytes::new(), true).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: page {page} at maxResults={page_size} must be served, got {} with \
             {:?}",
            answer.status, answer.body
        );
        let records = answer.body["queries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            i32::try_from(records.len()).unwrap_or(i32::MAX) <= page_size,
            "case {case:?}: page {page} must be cut at maxResults={page_size}, got {} records",
            records.len()
        );
        walked.extend(records);
        match answer.body["nextToken"].as_str() {
            None => break,
            Some(next) => token = Some(next.to_string()),
        }
    }
    walked
}

/// The exported document of one inquiry, citations included, fetched from the URL the
/// export announced exactly as a client fetches it: the path that URL names, with no
/// credential at all.
async fn exported_document(router: &MultiServiceRouter, case: &str, inquiry_id: &str) -> Value {
    let body = json!({ "complianceInquiryId": inquiry_id, "includeCitations": true });
    let exported = send(
        router,
        "POST",
        "/v1/compliance-inquiry/export",
        Bytes::from(body.to_string()),
        true,
    )
    .await;
    assert_eq!(
        exported.status, 200,
        "case {case:?}: the export must succeed, got {} with {:?}",
        exported.status, exported.body
    );
    let url = exported.body["documentPresignedUrl"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the export must announce a document URL, got {:?}",
                exported.body
            )
        })
        .to_string();
    let fetched = send(router, "GET", &fetchable(&url), Bytes::new(), false).await;
    assert_eq!(
        fetched.status, 200,
        "case {case:?}: the announced URL must serve the exported document, got {} with {:?}",
        fetched.status, fetched.body
    );
    fetched.body
}

/// Assert one `createdAt` carries the wire form its target declares — `TimestampAttribute`
/// is `@timestampFormat date-time`, so an RFC 3339 STRING and never restJson1's default
/// epoch-seconds number.
fn assert_date_time(case: &str, position: usize, value: &Value) {
    assert!(
        !value.is_number(),
        "case {case:?}: query {position} must report createdAt as an RFC 3339 string, got \
         {value:?}"
    );
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: query {position} must report createdAt, got {value:?}")
    });
    assert!(
        text.contains('T') && text.ends_with('Z'),
        "case {case:?}: query {position} must report createdAt as a UTC date-time, got {text:?}"
    );
}

/// One submission, and the query records the inquiry it creates must hold.
struct Record {
    label: &'static str,
    body: Value,
    /// The text each `QuerySummary` must report, in `queryIdentifier` order: a text
    /// submission submits the query it carried, and an uploaded document is processed
    /// section by section ("List of file sections/sheets to process"), one query each.
    reported: Vec<&'static str>,
}

/// The submissions the query-history rule is asserted over. The rows differ along the FORM
/// of the submission, how many queries it submits (one, two, three) and whether the
/// catalog holds anything bearing on them — none of which changes that every query the
/// inquiry holds is reported.
fn records() -> Vec<Record> {
    vec![
        Record {
            label: "a text query the catalog answers",
            body: text_body("Certification", "Which ISO 27001 certification applies?"),
            reported: vec!["Which ISO 27001 certification applies?"],
        },
        Record {
            label: "a text query the catalog holds nothing bearing on",
            body: text_body("Unknown", "zzqqxx wibblewobble nonexistentcomplianceterm"),
            reported: vec!["zzqqxx wibblewobble nonexistentcomplianceterm"],
        },
        Record {
            label: "an uploaded document naming one section",
            body: file_body("One section", &["SOC 2 Type 2 controls"]),
            reported: vec!["SOC 2 Type 2 controls"],
        },
        Record {
            label: "an uploaded document naming two sections",
            body: file_body("Two sections", &["ISO 27001 scope", "zzqqxx wibblewobble"]),
            reported: vec!["ISO 27001 scope", "zzqqxx wibblewobble"],
        },
        Record {
            label: "an uploaded document naming three sections",
            body: file_body(
                "Three sections",
                &["PCI DSS cardholder data", "FedRAMP Moderate boundary", "Outposts"],
            ),
            reported: vec![
                "PCI DSS cardholder data",
                "FedRAMP Moderate boundary",
                "Outposts",
            ],
        },
    ]
}

#[tokio::test]
async fn the_history_reports_one_record_per_query_the_inquiry_holds() {
    let router = emulator();

    for case in records() {
        let label = case.label;
        let id = create(&router, &case.body).await;
        let queries = settled_queries(&router, label, &id).await;

        assert_eq!(
            queries.len(),
            case.reported.len(),
            "case {label:?}: the history must report one record per query the inquiry holds, \
             expected {} records, got {queries:?}",
            case.reported.len()
        );

        for (position, expected_query) in case.reported.iter().enumerate() {
            let query = &queries[position];
            // The service's own order: "Sequential identifier of the query within the
            // inquiry", 1-based and contiguous, which is also the order the records arrive
            // in.
            assert_eq!(
                query["queryIdentifier"].as_i64(),
                Some(position as i64 + 1),
                "case {label:?}: query {position} must report its sequential identifier, got \
                 {query:?}"
            );
            assert_eq!(
                query["query"].as_str(),
                Some(*expected_query),
                "case {label:?}: query {position} must report the query text it was submitted \
                 as, got {query:?}"
            );
            let text = query["query"].as_str().unwrap_or_default();
            assert!(
                !text.is_empty() && text.chars().count() <= LONG_STRING_MAX
                    && !text.contains(['<', '>']),
                "case {label:?}: query {position} must report a query inside \
                 LongStringAttribute's bounds, got {text:?}"
            );
            let status = query["status"].as_str().unwrap_or_default();
            assert!(
                QUERY_STATUSES.contains(&status),
                "case {label:?}: query {position} must report one of QueryStatus's values \
                 {QUERY_STATUSES:?}, got {status:?}"
            );
            assert_ne!(
                status, "PROCESSING",
                "case {label:?}: query {position} must have left PROCESSING once its \
                 processing was due, got {query:?}"
            );
            let message = query["statusMessage"].as_str().unwrap_or_default();
            assert!(
                QUERY_STATUS_MESSAGES.contains(&message),
                "case {label:?}: query {position} must report one of QueryStatusMessage's \
                 values {QUERY_STATUS_MESSAGES:?}, got {message:?}"
            );
            assert_date_time(label, position, &query["createdAt"]);
        }

        // A read changes nothing a later caller sees, and the collection is the same
        // whatever page size it is walked at: a page cut at `maxResults` reports the same
        // records, in the same order, as the unpaged read.
        let reread = list_queries(&router, &id).await;
        assert_eq!(
            reread, queries,
            "case {label:?}: reading the history twice must report the same records, got \
             {reread:?} after {queries:?}"
        );
        for page_size in [1, 2, 300] {
            let walked = walk_queries(&router, label, &id, page_size).await;
            assert_eq!(
                walked, queries,
                "case {label:?}: a walk at maxResults={page_size} must enumerate the whole \
                 history, got {walked:?} where the unpaged read reports {queries:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_citation_is_one_triple_in_every_projection_of_the_query() {
    let router = emulator();
    let reports = catalog(&router).await;

    // One submission per FORM, each carrying terms several catalog documents share, so the
    // answer cites more than one source and the triples can be told apart.
    for case in [
        Record {
            label: "a text query citing several sources",
            body: text_body(
                "Audit",
                "Which independent auditor assessed the SOC 2 and ISO 27001 controls?",
            ),
            reported: vec![
                "Which independent auditor assessed the SOC 2 and ISO 27001 controls?",
            ],
        },
        Record {
            label: "an uploaded document citing several sources",
            body: file_body(
                "Questionnaire",
                &["SOC 2 Type 2 controls", "ISO 27001 certification scope"],
            ),
            reported: vec!["SOC 2 Type 2 controls", "ISO 27001 certification scope"],
        },
    ] {
        let label = case.label;
        let id = create(&router, &case.body).await;
        let queries = settled_queries(&router, label, &id).await;
        assert_eq!(
            queries.len(),
            case.reported.len(),
            "case {label:?}: expected {} records, got {queries:?}",
            case.reported.len()
        );

        let mut cited = 0usize;
        for (position, query) in queries.iter().enumerate() {
            let citations = query["citations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for (index, citation) in citations.iter().enumerate() {
                cited += 1;
                let source_label = citation["sourceLabel"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must label its source, got {citation:?}")
                });
                let source_content = citation["sourceContent"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must quote its source, got {citation:?}")
                });
                let source_link = citation["sourceLink"].as_str().unwrap_or_else(|| {
                    panic!("case {label:?}: a citation must link to its source, got {citation:?}")
                });

                // The triple is ONE source: the label names a report this catalog
                // publishes, the content is text that same report publishes, and the link
                // LOCATES that very report — not another one the answer also cited.
                let report = published(&reports, source_label);
                let description = report["description"].as_str().unwrap_or_default();
                let name = report["name"].as_str().unwrap_or_default();
                assert!(
                    description.contains(source_content) || name.contains(source_content),
                    "case {label:?}: query {position} citation {index} must quote text its \
                     labelled source publishes, got {source_content:?} for {source_label:?}"
                );
                let followed =
                    send(&router, "GET", &fetchable(source_link), Bytes::new(), true).await;
                assert_eq!(
                    followed.status, 200,
                    "case {label:?}: query {position} citation {index} must link to a source \
                     this emulator serves, got {} with {:?}",
                    followed.status, followed.body
                );
                assert_eq!(
                    (
                        followed.body["reportDetails"]["id"].clone(),
                        followed.body["reportDetails"]["name"].clone(),
                        followed.body["reportDetails"]["version"].clone(),
                    ),
                    (
                        report["id"].clone(),
                        report["name"].clone(),
                        report["version"].clone(),
                    ),
                    "case {label:?}: query {position} citation {index} must link to the source \
                     its label names and its content quotes, got {source_link:?} reading {:?}",
                    followed.body
                );
            }
        }
        assert!(
            cited > 1,
            "case {label:?}: the submission must be answered out of more than one source for \
             the triples to be told apart, got {queries:?}"
        );

        // Every projection of a citation reports the SAME triple: the exported document's
        // citations are the listing's citations, query by query and in citation order.
        let document = exported_document(&router, label, &id).await;
        let exported_queries = document["queries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            exported_queries.len(),
            queries.len(),
            "case {label:?}: the export must cover every query the inquiry holds, got {:?}",
            document["queries"]
        );
        for (position, query) in queries.iter().enumerate() {
            assert_eq!(
                exported_queries[position]["citations"],
                query["citations"],
                "case {label:?}: query {position} must report the same citation triples in the \
                 exported document as in the listing, got {:?} against {:?}",
                exported_queries[position]["citations"],
                query["citations"]
            );
        }
    }
}
