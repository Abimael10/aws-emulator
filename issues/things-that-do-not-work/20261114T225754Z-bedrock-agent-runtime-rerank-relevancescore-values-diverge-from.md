Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-runtime-rerank-inline-text-documents
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-runtime-rerank-inline-text-documents
Issue-Shape-Id: com.amazonaws.bedrockagentruntime#AmazonBedrockAgentRunTimeService
Issue-Key: bedrock-agent-runtime|Rerank|2xx->2xx|$.results[*].relevanceScore
Issue-Commit: fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8

# bedrock-agent-runtime: Rerank relevanceScore values diverge from the reranker model's scale

> _Note: this issue was identified against the code base at commit `fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Rerank

## Description
Single cause (Cause 1 of 1): Rerank returns 200 with the correct result set and the correct ordering, but every result's `relevanceScore` is a different number than the service produces for the same query/documents, and the whole scale is shifted.

AWS: Smithy model (com.amazonaws.bedrockagentruntime, operation Rerank, POST /rerank, output RerankResponse.results -> RerankResultsList -> RerankResult) makes `relevanceScore` a REQUIRED Float with doc "The relevance score of the document." and declares no range, so the model states no numeric rule — recorded behavior only for the values themselves. What the recording shows is that the score of each (query, document) pair is produced by the reranker model named in `rerankingConfiguration.bedrockRerankingConfiguration.modelConfiguration.modelArn`, and that for a fixed query, a fixed set of inline TEXT source documents and a fixed reranker model the service returns a STABLE score per result: the recorded response is reproducible, strictly descending over `results`, and spans roughly five decades from the best match down to the weakest (recorded top-ranked result ~2.6e-02, second ~3.4e-04, third ~2.5e-06). Those three figures are the recording's values, not the requirement; the requirement is that the score be the reranker model's score for that pair, so replaying the same request reproduces the same numbers.

Emulator: for the same request it returns a different score for EVERY result — the replay reports $.results[0].relevanceScore expected 0.0261556189507246, got 0.14660696685314178; $.results[1].relevanceScore expected 0.00033930179779417813, got 0.000974746304564178; $.results[2].relevanceScore expected 2.541350113460794e-06, got 6.1442124206223525e-06. Status (200), result count, `index` values and the descending order all match; only the scores diverge, and they diverge non-uniformly (the top result is off by ~5.6x, the tail by ~2.4x), so this is not a single scale factor that a client could compensate for. The cause is visible in the handler: services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/src/ops/rerank.rs scores a pair with `relevance_score()` = exp(-SCORE_DECAY * (1 - relevance)) over a lexical relevance built from stemmed-term COVERAGE_SHARE/DENSITY_SHARE (SCORE_DECAY = 12.0, COVERAGE_SHARE = 0.8), and its own comment states the decay figure is "EMULATOR-CHOSEN". The chosen decay and the coverage/density blend together place the curve on a different scale than the one the recorded responses occupy, so no recorded Rerank score is reproduced.

Why: a client that only re-orders its candidates is unaffected, but `relevanceScore` is a required output field that callers consume numerically — the standard RAG pattern is to drop or down-weight results below a fixed relevance threshold, or to feed the score into a blend with another retriever's score. Against the emulator those thresholds select a different (here, larger) set of documents than against AWS: a weak match scoring 0.1466 passes a 0.1 cut-off that the service's 0.0262 for the same document fails. Code tuned or tested against the emulator therefore ships with a cut-off that behaves differently in production, and a regression test that pins an expected score from a recorded AWS response can never pass.

Fix: the Rerank handler's scoring in services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/src/ops/rerank.rs — specifically `relevance_score()` and the SCORE_DECAY / COVERAGE_SHARE / DENSITY_SHARE constants that define the curve. The response serializer and the handler's ordering, index assignment and validation are all correct and need no change; what is wrong is the function mapping a (query, document) pair to its score, which must reproduce the reranker model's scores for the recorded corpus rather than an emulator-chosen logarithmic heuristic.

Traces: bedrock-agent-runtime-rerank-inline-text-documents#task#1 (the only net-new divergence in this service; the other two clusters in this replay, GetDocumentContent/userContext and Retrieve/unknown knowledge base, are already owned by open issues 20261111T002352Z-bedrock-agent-runtime-getdocumentcontent-rejects-the-modeled-usercontext.md and 20261109T032116Z-bedrock-agent-runtime-retrieve-s-unknown-knowledge-base.md and are not re-filed here; open issue 20261029T171944Z-bedrock-agent-runtime-ten-net-new-causes-session.md covers unrelated session/invocation causes and does not cover Rerank scoring).

## Evidence
Fixture bedrock-agent-runtime-rerank-inline-text-documents, step task#1, operation Rerank, verdict mismatch.
Expected status 200, actual status 200 (status agrees).
Diverging fields (expected -> actual):
  $.results[0].relevanceScore: 0.0261556189507246 -> 0.14660696685314178 (~5.6x high)
  $.results[1].relevanceScore: 0.00033930179779417813 -> 0.000974746304564178 (~2.9x high)
  $.results[2].relevanceScore: 2.541350113460794e-06 -> 6.1442124206223525e-06 (~2.4x high)
No other field diverged: result count, each result's `index`, and the descending ordering all matched, so the divergence is confined to the score value and not to ranking or shape.
Smithy: RerankResult.relevanceScore is `required` Float (target com.amazonaws.bedrockagentruntime#Float), doc "The relevance score of the document.", no @range; Rerank is POST /rerank returning 200.
Emulator source: src/ops/rerank.rs `relevance_score()` returns (-SCORE_DECAY * (1.0 - relevance)).exp() with `const SCORE_DECAY: f64 = 12.0` documented as "EMULATOR-CHOSEN — no reranker model runs here to publish one", and `relevance()` = 0.8 * query-term coverage + 0.2 * document-term density over Snowball-stemmed terms.

## Steps to Reproduce
1. git_clone() and boot the emulator built from the checkout.
2. Replay the wire corpus (replay_wire_corpus) and look at fixture bedrock-agent-runtime-rerank-inline-text-documents, step task#1.
3. Equivalently, by hand: POST /rerank to the bedrock-agent-runtime endpoint with the fixture's request — a single entry in `queries` (type TEXT) and three inline TEXT entries in `sources`, with `rerankingConfiguration.type` = BEDROCK_RERANKING_MODEL and a valid `arn:aws:bedrock:<region>::foundation-model/<model-id>` reranker `modelArn`.
4. Observe 200 with three results in the expected order, but each result's `relevanceScore` differing from the recorded response (0.14660696685314178 / 0.000974746304564178 / 6.1442124206223525e-06 instead of 0.0261556189507246 / 0.00033930179779417813 / 2.541350113460794e-06).
5. Re-issue the identical request: the emulator's scores are stable, confirming the divergence is the scoring function and not per-run nondeterminism.
