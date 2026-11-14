// SPDX-License-Identifier: CC0-1.0

//! What `Rerank` RANKS BY, and which `modelArn` names a reranker at all, asserted end to
//! end over the wire (`POST /rerank`).
//!
//! Every test drives real HTTP requests through the framework router over the shared
//! engine, reusing the request harness of `tests/all/sessions.rs` and the body builders
//! of `tests/all/reranking.rs` rather than a second copy of either, so the wire protocol
//! (restJson1), the boundary validator and the handler are all exercised and every
//! assertion reads a response BODY.
//!
//! The two rules under test, each over a TABLE of inputs:
//!
//! 1. The ranking is driven by how much of what the query ASKS ABOUT each document
//!    carries: a document covering more of the query's content-bearing terms outranks one
//!    covering fewer whatever either's length, and the reported `relevanceScore` DECAYS
//!    down the ranking — each rank at least an order of magnitude below the one above it,
//!    rather than a band of near-equal numbers — so two documents of different relevance
//!    never carry the same score. The query's function words rank nothing: a document
//!    that shares only those is no more relevant than one sharing nothing.
//! 2. A reranking `modelArn` names a Bedrock FOUNDATION MODEL
//!    (`arn:<partition>:bedrock:<region>::foundation-model/<model-id>`); anything else is
//!    refused `400 ValidationException` with one fixed sentence, before any ranking is
//!    computed, and no results are served.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::reranking::{rerank_body, text_source, MODEL};
use crate::sessions::{call, error_code, router};

/// RULE 2: the ONE sentence a `modelArn` that names no Bedrock foundation model is
/// refused with — entirely fixed text, including the format it states, where the
/// angle-bracketed field names are part of the sentence rather than values. The rejected
/// value is not echoed and no member path is named.
const INVALID_MODEL_ARN: &str = "The provided model ARN for reranking is invalid. Please, use a \
                                 valid model ARN. Also check the model ARN follows this format: \
                                 arn:<partition>:bedrock:<region>::foundation-model/<model-id>";

/// How much a score has to fall from one rank to the next to count as DECAYING rather
/// than crowding: an order of magnitude. The rule is about separation, not about any
/// particular number, so every row below is well clear of this.
const DECAY_PER_RANK: f64 = 10.0;

/// Rerank `sources` against `query` with the reranking model `model_arn`.
async fn rerank_with_model(
    router: &MultiServiceRouter,
    model_arn: &str,
    query: &str,
    sources: &[Value],
) -> (u16, Value) {
    let mut body = rerank_body(query, sources, None);
    body["rerankingConfiguration"]["bedrockRerankingConfiguration"]["modelConfiguration"]
        ["modelArn"] = json!(model_arn);
    call(router, "POST", "/rerank", body).await
}

/// The `(index, relevanceScore)` pairs of a successful rerank, in the order reported.
async fn ranking(
    router: &MultiServiceRouter,
    case: &str,
    query: &str,
    sources: &[Value],
) -> Vec<(i64, f64)> {
    let (status, body) = rerank_with_model(router, MODEL, query, sources).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Rerank to answer 200, got {status} {body:?}"
    );
    body["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no results in {body:?}"))
        .iter()
        .map(|result| {
            let index = result["index"]
                .as_i64()
                .unwrap_or_else(|| panic!("case {case:?}: no index in {result:?}"));
            let score = result["relevanceScore"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: no relevanceScore in {result:?}"));
            (index, score)
        })
        .collect()
}

/// RULE 1: the sources come back ordered by how much of the query each document answers,
/// and the scores decay by at least [`DECAY_PER_RANK`] from each rank to the next.
///
/// Each row is one query with its documents listed in the order the rule requires them
/// back — most relevant first — sent in a DIFFERENT order, so a row can only pass by
/// ranking. The rows differ along what the rule generalises over: how many relevance
/// levels there are, the documents' lengths, whether the distractor shares the query's
/// function words or its content words, which inflection a document spells a query term
/// in, and how far from first the answer was sent.
#[tokio::test]
async fn the_scores_decay_down_a_ranking_driven_by_query_coverage() {
    let router = router();
    // (case, query, the documents in the order the rule requires them back, the order
    // they are sent in as positions into that list)
    let cases: Vec<(&str, &str, Vec<&str>, Vec<usize>)> = vec![
        (
            "three relevance levels",
            "polar bear population survey",
            vec![
                "Polar bear population surveys counted fewer bears.",
                "The bear population of the valley grew.",
                "Sea ice extent declined in the Arctic.",
            ],
            vec![2, 0, 1],
        ),
        (
            "the distractor shares only the query's function words",
            "how does a reverse proxy cache responses",
            vec![
                "A reverse proxy caches responses close to the client.",
                "How does a day at the beach sound.",
            ],
            vec![1, 0],
        ),
        (
            "a long answer outranks a short distractor",
            "ledger reconciliation schedule",
            vec![
                "The ledger reconciliation schedule, agreed last quarter by the finance and \
                 treasury teams, runs nightly and is reviewed every month.",
                "A schedule was pinned to the wall.",
            ],
            vec![1, 0],
        ),
        (
            "the answer spells the query's terms in another inflection",
            "migrating bird tagging",
            vec![
                "Migration of tagged birds was recorded.",
                "A tag was sewn into the jacket.",
                "Concrete cures in twenty-eight days.",
            ],
            vec![2, 1, 0],
        ),
        (
            "four relevance levels",
            "solar panel inverter fault",
            vec![
                "Solar panel inverter faults tripped the array.",
                "The solar panel array was cleaned.",
                "A fault was logged by the controller.",
                "Bread dough needs proving twice.",
            ],
            vec![3, 2, 1, 0],
        ),
    ];

    for (case, query, ordered, sent_order) in cases {
        let sources: Vec<Value> = sent_order
            .iter()
            .map(|position| text_source(ordered[*position]))
            .collect();
        let ranked = ranking(&router, case, query, &sources).await;
        assert_eq!(
            ranked.len(),
            sources.len(),
            "case {case:?}: expected every source ranked once, got {ranked:?}"
        );

        // The documents come back in the order the rule requires, translated back into
        // the positions they were SENT at.
        let expected: Vec<i64> = ordered
            .iter()
            .enumerate()
            .map(|(rank, _)| {
                sent_order
                    .iter()
                    .position(|position| *position == rank)
                    .map(|sent_at| sent_at as i64)
                    .unwrap_or_else(|| panic!("case {case:?}: rank {rank} was not sent"))
            })
            .collect();
        let reported: Vec<i64> = ranked.iter().map(|(index, _)| *index).collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the sources ranked {expected:?}, got {ranked:?}"
        );

        for pair in ranked.windows(2) {
            let (above, below) = (pair[0].1, pair[1].1);
            assert!(
                below > 0.0,
                "case {case:?}: expected every relevanceScore above zero, got {ranked:?}"
            );
            assert!(
                above >= below * DECAY_PER_RANK,
                "case {case:?}: expected rank {above} to be at least {DECAY_PER_RANK}x the \
                 rank below it, got {below} in {ranked:?}"
            );
        }
    }
}

/// RULE 1 at the ends of the scale: a document that shares only the query's FUNCTION
/// words is no more relevant than one that shares nothing at all, while one that shares a
/// content term is — so what the ordering is driven by is what the query asks about, not
/// how many of its words a document happens to repeat.
#[tokio::test]
async fn sharing_only_the_querys_function_words_ranks_nothing() {
    let router = router();
    const QUERY: &str = "when does the ferry to the island leave";
    // (case, the document, whether the rule requires it ranked above a document sharing
    // nothing with the query)
    let cases: [(&str, &str, bool); 4] = [
        (
            "only the query's function words",
            "When does it come up, and does it go down.",
            false,
        ),
        ("nothing in common", "Granite was quarried here.", false),
        (
            "one content term",
            "A granite island rises offshore.",
            true,
        ),
        (
            "every content term",
            "The ferry to the island leaves hourly.",
            true,
        ),
    ];

    let mut floor: Option<(&str, f64)> = None;
    let mut above: Vec<(&str, f64)> = Vec::new();
    for (case, document, ranks) in cases {
        let ranked = ranking(&router, case, QUERY, &[text_source(document)]).await;
        let (_, score) = ranked[0];
        if ranks {
            above.push((case, score));
            continue;
        }
        if let Some((first_case, first)) = floor {
            assert!(
                (score - first).abs() < 1e-6,
                "case {case:?}: expected the same floor as case {first_case:?} ({first}), \
                 got {score}"
            );
        } else {
            floor = Some((case, score));
        }
    }

    let (floor_case, floor) = floor.expect("the table carries a document that ranks nothing");
    for (case, score) in above {
        assert!(
            score > floor * DECAY_PER_RANK,
            "case {case:?}: expected a score well above the floor of case {floor_case:?} \
             ({floor}), got {score}"
        );
    }
}

/// RULE 2: a `modelArn` that names a Bedrock foundation model is SERVED; anything else is
/// refused with the one fixed sentence and no results.
///
/// The accepted rows differ along partition, region and model id. The refused rows differ
/// along how the value fails to name a published foundation model: a bare model id, an
/// account-scoped ARN (a foundation model is published by the service, so its ARN carries
/// no account), another service's ARN, another resource type, a value that is no ARN at
/// all, and an ARN naming no model. Every value here satisfies the modelled
/// `BedrockModelArn` pattern
/// (`smithy_query(query="shape",shape="BedrockModelArn")`), so what each row exercises is
/// the handler's resolution rather than the boundary validator's.
#[tokio::test]
async fn a_reranking_model_arn_must_name_a_bedrock_foundation_model() {
    let router = router();
    let sources = [
        text_source("Tide tables for the strait."),
        text_source("The strait freezes in January."),
    ];

    let accepted: [(&str, &str); 4] = [
        ("the commercial partition", MODEL),
        (
            "another region and model id",
            "arn:aws:bedrock:us-east-1::foundation-model/cohere.rerank-v3-5:0",
        ),
        (
            "the GovCloud partition",
            "arn:aws-us-gov:bedrock:us-gov-west-1::foundation-model/amazon.rerank-v1:0",
        ),
        (
            "the China partition",
            "arn:aws-cn:bedrock:cn-north-1::foundation-model/amazon.rerank-v1:0",
        ),
    ];
    for (case, model_arn) in accepted {
        let (status, body) = rerank_with_model(&router, model_arn, "strait tides", &sources).await;
        assert_eq!(
            (status, error_code(&body)),
            (200, ""),
            "case {case:?}: expected {model_arn} to name a reranker, got {status} {body:?}"
        );
        assert_eq!(
            body["results"].as_array().map(Vec::len),
            Some(sources.len()),
            "case {case:?}: expected every source ranked, got {body:?}"
        );
    }

    let refused: [(&str, &str); 6] = [
        ("a bare model id", "amazon.rerank-v1:0"),
        (
            "an account-scoped ARN",
            "arn:aws:bedrock:us-west-2:000000000000:foundation-model/amazon.rerank-v1:0",
        ),
        (
            "another service's ARN",
            "arn:aws:sagemaker:us-west-2::foundation-model/amazon.rerank-v1:0",
        ),
        (
            "another resource type",
            "arn:aws:bedrock:us-west-2::custom-model/amazon.rerank-v1:0",
        ),
        ("no ARN at all", "rerank-model"),
        (
            "an ARN naming no model",
            "arn:aws:bedrock:us-west-2::foundation-model/",
        ),
    ];
    for (case, model_arn) in refused {
        let (status, refusal) =
            rerank_with_model(&router, model_arn, "strait tides", &sources).await;
        assert_eq!(
            (status, error_code(&refusal)),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException for {model_arn}, got {status} \
             {refusal:?}"
        );
        assert_eq!(
            refusal.get("message").and_then(Value::as_str),
            Some(INVALID_MODEL_ARN),
            "case {case:?}: expected the fixed refusal, got {refusal:?}"
        );
        assert!(
            refusal.get("results").is_none(),
            "case {case:?}: a refused request must carry no ranking, got {refusal:?}"
        );
    }
}
