// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `Retrieve` (`POST /knowledgebases/{knowledgeBaseId}/retrieve`)
//! BOUNDS the ranking it reports.
//!
//! THE RULE UNDER TEST: a retrieval ranks the chunks of the knowledge base once, and
//! `numberOfResults` only says how much of that one ranking the response carries — the
//! response for a bound of `n` is, result for result, the LEADING `n` of the response the
//! largest bound gives, so no bound changes which chunk ranks where, what each result cites
//! or what it scores. A bound outside the range the model publishes for that member is
//! refused with the modelled `ValidationException` (400) naming it, never clamped to the
//! nearest value the service would serve. `overrideSearchType` selects between two
//! strategies over the same vectors, so naming either one names that same ranking.
//!
//! The rows span what the rule generalises over: every bound from one result to one past
//! the number of chunks the knowledge base holds, both published ends of the member's
//! range and one value past each of them, and each value of the search-type override
//! beside a request naming none.
//!
//! The harness is the sibling retrieval suite's ([`crate::knowledge_base_retrieval`]), so
//! the knowledge base here is one a caller really created and the chunks are the ones an
//! ingestion really recorded.

use serde_json::{json, Value};

use crate::knowledge_base_retrieval::Emulator;
use crate::sessions::{call, error_code};

/// The ends of the range the model publishes for `numberOfResults`
/// (`smithy_query(query="shape",shape="KnowledgeBaseVectorSearchConfiguration")` ->
/// `numberOfResults` range 1..100).
const MIN_NUMBER_OF_RESULTS: i64 = 1;
const MAX_NUMBER_OF_RESULTS: i64 = 100;

/// The member path a refusal of that range names.
const NUMBER_OF_RESULTS_MEMBER: &str =
    "retrievalConfiguration.vectorSearchConfiguration.numberOfResults";

/// Every value of the modelled search-type override
/// (`smithy_query(query="shape",shape="SearchType")` -> HYBRID | SEMANTIC).
const SEARCH_TYPES: [&str; 2] = ["HYBRID", "SEMANTIC"];

/// The query every retrieval here asks, sharing terms with some documents and not others,
/// so the ranking it produces is not the order the chunks were ingested in.
const QUERY: &str = "which launch code does the propulsion team hold";

/// The source documents the suite's data source holds: six objects on different topics and
/// of different lengths, so the knowledge base holds more chunks than the model's default
/// page and a bound shorter than the ranking really drops something.
const DOCUMENTS: [(&str, &str); 6] = [
    (
        "codes.txt",
        "The Thunderbird project launch code is TBX-9 and the propulsion team holds it.",
    ),
    (
        "handover.txt",
        "The propulsion team hands the launch code over to the duty officer at midnight.",
    ),
    (
        "menu.txt",
        "The cafeteria serves lentil soup on Tuesday and roast squash on Thursday.",
    ),
    ("inventory.txt", "The spare battery packs live in bay 4."),
    (
        "roster.txt",
        "Ana works Monday, Bo works Friday and Cal works Sunday.",
    ),
    (
        "weather.txt",
        "Winds stay light and variable at the pad until Thursday afternoon.",
    ),
];

/// One knowledge base over one ingested S3 data source holding [`DOCUMENTS`].
struct Knowledge {
    emulator: Emulator,
    id: String,
}

impl Knowledge {
    /// Create the knowledge base, attach the data source over `bucket` and run its
    /// ingestion to completion, so every document is a chunk a retrieval can report.
    async fn new(index: &str, bucket: &str) -> Self {
        let emulator = Emulator::new(index);
        for (key, text) in DOCUMENTS {
            emulator.put_object(bucket, key, text);
        }
        let id = emulator.knowledge_base("result-bounds", index).await;
        let data_source = emulator.data_source(&id, "s3-docs", bucket, None).await;
        emulator.ingest(&id, &data_source).await;
        Self { emulator, id }
    }

    /// `POST /knowledgebases/{id}/retrieve` with [`QUERY`] and the
    /// `retrievalConfiguration` member `configuration`, answering the raw `(status, body)`.
    /// A `Value::Null` configuration names none at all.
    async fn retrieve(&self, configuration: Value) -> (u16, Value) {
        let mut body = json!({ "retrievalQuery": { "text": QUERY } });
        if !configuration.is_null() {
            body["retrievalConfiguration"] = configuration;
        }
        call(
            &self.emulator.router,
            "POST",
            &format!("/knowledgebases/{}/retrieve", self.id),
            body,
        )
        .await
    }

    /// [`Self::retrieve`], asserting the call succeeded, and answer `retrievalResults`.
    async fn results(&self, case: &str, configuration: Value) -> Vec<Value> {
        let (status, body) = self.retrieve(configuration).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Retrieve to answer 200, got {status} {body:?}"
        );
        body["retrievalResults"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no retrievalResults in {body:?}"))
            .clone()
    }

    /// Every chunk the knowledge base holds, ranked, read with the largest bound the model
    /// admits: the ONE ranking every bounded response below is a leading slice of.
    async fn whole_ranking(&self) -> Vec<Value> {
        let case = "the whole ranking";
        let results = self
            .results(
                case,
                json!({
                    "vectorSearchConfiguration": { "numberOfResults": MAX_NUMBER_OF_RESULTS },
                }),
            )
            .await;
        assert_eq!(
            results.len(),
            DOCUMENTS.len(),
            "case {case:?}: expected one result per ingested document, got {}",
            results.len()
        );
        let mut previous: Option<f64> = None;
        for result in &results {
            let score = result["score"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: no numeric score in {result:?}")
            });
            if let Some(previous) = previous {
                assert!(
                    score <= previous,
                    "case {case:?}: expected the ranking to descend, got {score} after \
                     {previous}"
                );
            }
            previous = Some(score);
        }
        results
    }
}

/// RULE: for every bound, the response is the leading `n` results of the one ranking —
/// same chunks, same order, same citations and same scores — and a bound past the number
/// of chunks the knowledge base holds reports all of them rather than padding the answer.
#[tokio::test]
async fn every_result_bound_reports_the_leading_results_of_one_ranking() {
    let knowledge = Knowledge::new("leading-index", "retrieval-leading-docs").await;
    let whole = knowledge.whole_ranking().await;

    // Every bound from the published minimum to one past the number of chunks held, plus
    // the published maximum: a bound at or above the count reports the whole ranking.
    let bounds: Vec<i64> = (MIN_NUMBER_OF_RESULTS..=DOCUMENTS.len() as i64 + 1)
        .chain([MAX_NUMBER_OF_RESULTS])
        .collect();
    for bound in bounds {
        let case = format!("numberOfResults {bound}");
        let results = knowledge
            .results(
                &case,
                json!({ "vectorSearchConfiguration": { "numberOfResults": bound } }),
            )
            .await;
        let kept = (bound as usize).min(whole.len());
        assert_eq!(
            results.len(),
            kept,
            "case {case:?}: expected {kept} results, got {}",
            results.len()
        );
        assert_eq!(
            results,
            whole[..kept].to_vec(),
            "case {case:?}: expected the leading {kept} results of the one ranking"
        );
    }
}

/// RULE: a `numberOfResults` outside the range the model publishes is refused with the
/// modelled `ValidationException` (400) naming the member, never clamped to a bound the
/// service would serve; each published end of that range IS served.
#[tokio::test]
async fn a_result_bound_outside_the_published_range_is_refused_not_clamped() {
    let knowledge = Knowledge::new("range-index", "retrieval-range-docs").await;
    let whole = knowledge.whole_ranking().await;

    // (case, the bound, how many results it reports — `None` for a refusal)
    let cases: [(&str, i64, Option<usize>); 6] = [
        (
            "the published minimum",
            MIN_NUMBER_OF_RESULTS,
            Some(MIN_NUMBER_OF_RESULTS as usize),
        ),
        (
            "the published maximum",
            MAX_NUMBER_OF_RESULTS,
            Some(DOCUMENTS.len()),
        ),
        ("one below the minimum", MIN_NUMBER_OF_RESULTS - 1, None),
        ("one above the maximum", MAX_NUMBER_OF_RESULTS + 1, None),
        ("a negative bound", -7, None),
        ("far above the maximum", 100_000, None),
    ];
    for (case, bound, served) in cases {
        let configuration = json!({ "vectorSearchConfiguration": { "numberOfResults": bound } });
        match served {
            Some(expected) => {
                let results = knowledge.results(case, configuration).await;
                assert_eq!(
                    results,
                    whole[..expected].to_vec(),
                    "case {case:?}: expected the leading {expected} results of the one \
                     ranking, got {} results",
                    results.len()
                );
            }
            None => {
                let (status, body) = knowledge.retrieve(configuration).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the modelled 400 for {bound}, got {status} \
                     {body:?}"
                );
                assert_eq!(
                    error_code(&body),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {body:?}"
                );
                let message = body["message"].as_str().unwrap_or_default();
                assert!(
                    message.contains(NUMBER_OF_RESULTS_MEMBER),
                    "case {case:?}: expected the refusal to name \
                     {NUMBER_OF_RESULTS_MEMBER:?}, got {message:?}"
                );
            }
        }
    }
}

/// RULE: `overrideSearchType` chooses between two strategies over the same vectors of the
/// same knowledge base, so a request naming either one is answered with the same ranking
/// as a request naming none.
#[tokio::test]
async fn the_search_type_override_names_the_same_one_ranking() {
    let knowledge = Knowledge::new("search-type-index", "retrieval-search-type-docs").await;
    let whole = knowledge.whole_ranking().await;

    for search_type in SEARCH_TYPES {
        let case = format!("overrideSearchType {search_type}");
        let results = knowledge
            .results(
                &case,
                json!({
                    "vectorSearchConfiguration": {
                        "numberOfResults": MAX_NUMBER_OF_RESULTS,
                        "overrideSearchType": search_type,
                    },
                }),
            )
            .await;
        assert_eq!(
            results, whole,
            "case {case:?}: expected the same ranking as a retrieval naming no search type"
        );
    }
}

/// RULE: a request naming no `retrievalConfiguration` at all takes the default page the
/// model puts on `numberOfResults`, which is the leading results of that same ranking.
#[tokio::test]
async fn a_request_naming_no_configuration_takes_the_modelled_default_page() {
    let knowledge = Knowledge::new("default-index", "retrieval-default-docs").await;
    let whole = knowledge.whole_ranking().await;
    // The `default` the model puts on `numberOfResults`
    // (`smithy_query(query="shape",shape="KnowledgeBaseVectorSearchConfiguration")`).
    let default_page = 5;
    assert!(
        default_page < whole.len(),
        "setup: expected the knowledge base to hold more chunks than the default page of \
         {default_page}, got {}",
        whole.len()
    );

    // (case, the `retrievalConfiguration` member the request carries)
    let cases: [(&str, Value); 3] = [
        ("no retrievalConfiguration at all", Value::Null),
        (
            "an empty vectorSearchConfiguration",
            json!({ "vectorSearchConfiguration": {} }),
        ),
        (
            "a vectorSearchConfiguration naming only a search type",
            json!({ "vectorSearchConfiguration": { "overrideSearchType": SEARCH_TYPES[1] } }),
        ),
    ];
    for (case, configuration) in cases {
        let results = knowledge.results(case, configuration).await;
        assert_eq!(
            results,
            whole[..default_page].to_vec(),
            "case {case:?}: expected the leading {default_page} results of the one ranking, \
             got {} results",
            results.len()
        );
    }
}
