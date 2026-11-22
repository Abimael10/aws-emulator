Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-runtime-rerank-inline-text-documents
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-runtime-rerank-inline-text-documents
Issue-Shape-Id: com.amazonaws.bedrockagentruntime#AmazonBedrockAgentRunTimeService
Issue-Key: bedrock-agent-runtime|Rerank|2xx->2xx|$.results[*].relevanceScore
Issue-Commit: 33eed1d27fdea9bbe5c40c61631ec81c0850a581

# Rerank relevanceScore values diverge from AWS: emulator's synthetic log score and its exp(-12) floor replace the reranker model's scores

> _Note: this issue was identified against the code base at commit `33eed1d27fdea9bbe5c40c61631ec81c0850a581`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Rerank

## Description
ONE cause.

Cause 1 of 1: Rerank returns the right result shape, the right `index` values and the right ordering, but every `relevanceScore` it reports is a different number from the one AWS reports for the same (query, document) pair, and the lowest-relevance document is additionally pinned to a floor that is ABOVE the score AWS gives it.

AWS: Smithy model `com.amazonaws.bedrockagentruntime#Rerank` -> output `RerankResponse.results` (`RerankResultsList` of `RerankResult`), whose members are `index` (`Integer`, @range min 0 max 1000, required), `relevanceScore` (`Float`, required, doc "The relevance score of the document.") and `document` (`RerankDocument`, sensitive). HTTP binding POST /rerank, success status 200. The model constrains neither the range nor the derivation of `relevanceScore`, and the docs say nothing beyond "The relevance score of the document" — so for the VALUE this is recorded behavior only. What the recording shows: for the fixture's single query and three inline text documents, AWS answers 200 with results in descending score order and scores `0.0261556189507246`, `0.00033930179779417813`, `2.541350113460794e-06`. That third value is a real, deterministic score for that pair, not a per-run artifact: it carries no request id, no account id, no timestamp and no generated name, and the same request recorded the same value. Note that the scores AWS reports span more than four decades and are NOT bounded below by any observable floor — `2.54e-06` is smaller than the smallest score the emulator can emit at all.

Emulator: the emulator answers 200 with the same three results in the same order and the same `index` values, but scores `0.14660696685314178`, `0.000974746304564178`, `6.1442124206223525e-06`. These are not the reranker model's scores; they are produced by the handler's own closed-form transform, which the handler documents as emulator-chosen: a lexical relevance in [0,1] (COVERAGE_SHARE 0.8 of the query's stemmed content terms the document carries, plus DENSITY_SHARE 0.2 of the document's terms that are those content terms) pushed through `exp(-SCORE_DECAY * (1 - relevance))` with SCORE_DECAY = 12.0. The consequence visible in the replay is twofold. (a) Scale: the top document's score is off by roughly 5.6x and the middle one by roughly 2.9x, because the emulator's lexical relevance for a pair is not the reranker's relevance for that pair. (b) Floor: the emulator's score has a hard positive minimum of `exp(-12)` = `6.1442124206223525e-06`, which is EXACTLY the value it emitted for the third document — i.e. that document scored relevance 0.0 and was clamped to the floor — whereas AWS reported `2.541350113460794e-06`, BELOW that floor. So the emulator's lowest possible score is ~2.4x higher than a score AWS actually returns, and every weakly-matching document collapses onto one identical constant instead of being separated.

Why: a client that filters or gates on the absolute `relevanceScore` rather than on rank gets the wrong answer. Thresholding (keep results with `relevanceScore >= 0.05`, a common reranker pattern) keeps the emulator's top document and drops AWS's; a client that treats "score below 1e-05" as "irrelevant, do not cite" drops AWS's third document but keeps the emulator's, because the emulator cannot emit a score below `exp(-12)`. A client that normalises scores (divides by the top score, or sums them into a confidence) reads a materially different distribution. Code calibrated against real Bedrock reranker scores therefore behaves differently against the emulator even though the ordering it was testing matches.

Note: no neural reranker runs inside the emulator, so the exact scores of a real reranking model are not reproducible by construction; the handler's own module docs say as much. The actionable part of this divergence is the part that is a free choice rather than a model output: the SCALE (SCORE_DECAY / the coverage-density weighting, which sets how far the top-of-list score sits from 1.0) and above all the FLOOR, which is a hard clamp the emulator imposes and the recorded AWS response demonstrably violates. A zero-relevance document must be able to score below `exp(-12)`, and documents that share no query term must not all be mapped to one identical constant.

Fix: the scoring function of the Rerank handler, not its ordering, validation or serialization — those all match. Replace the hard `exp(-SCORE_DECAY * (1 - relevance))` clamp floor with a scale whose range reaches below the smallest score AWS is recorded emitting, and recalibrate the decay/weighting so a top-ranked match lands in the recorded order of magnitude instead of several times above it. Keep the existing per-pair purity (score a function of the one (query, document) pair only) and the existing ordering, which replay confirms are already right.

Traces: fixture `bedrock-agent-runtime-rerank-inline-text-documents`, step `task#1` (the only member of the cluster). Handler and the constants involved: services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/src/ops/rerank.rs lines 429-463 (SCORE_DECAY, COVERAGE_SHARE, DENSITY_SHARE, `relevance_score` and its `.clamp(0.0, 1.0)`), lines 490-508 (`QueryTerms::relevance`).

No existing open issue covers this. The open bedrock-agent-runtime issues are the two Lambda/s3vectors runtime issues and the GetExecutionFlowSnapshot / ListFlowExecutionEvents stub issues; none touches Rerank or its scoring.

## Evidence
Replay verdict: mismatch, expected_status 200, actual_status 200 (status and result shape agree; only the score values diverge).

divergence_key: service=bedrock-agent-runtime|Rerank|status=200->200|keys=$.results[0].relevanceScore,$.results[1].relevanceScore,$.results[2].relevanceScore

Per-field detail from the replay:
  $.results[0].relevanceScore: expected 0.0261556189507246,      got 0.14660696685314178   (~5.6x high)
  $.results[1].relevanceScore: expected 0.00033930179779417813,  got 0.000974746304564178  (~2.9x high)
  $.results[2].relevanceScore: expected 2.541350113460794e-06,   got 6.1442124206223525e-06 (~2.4x high)

Derivation of the emulator's three values, confirming they come from the handler's own transform and not from any model: with SCORE_DECAY = 12.0 and score = exp(-12 * (1 - relevance)),
  0.14660696685314178    <=> relevance = 0.84
  0.000974746304564178   <=> relevance = 0.42228
  6.1442124206223525e-06 <=> relevance = 0.0  -- i.e. exactly exp(-12), the clamp floor

The floor is the hard part of the divergence: exp(-12) = 6.1442124206223525e-06 is the SMALLEST score the emulator can emit for any (query, document) pair, yet AWS returned 2.541350113460794e-06 for the third document -- about 2.4x below it. Inverting the emulator's formula on the AWS values needs relevance = 0.69633, 0.33428 and -0.0736 respectively; the last is negative, so no input to the emulator's current scoring function can reproduce the recorded AWS score at all.

Agreeing (non-diverging) parts of the same response, for scope: HTTP 200, results length 3, all three `index` values, the descending-score ordering, and the echoed `document` payloads all matched.

Model grounding: Smithy RerankResult.relevanceScore -> Float, required, doc "The relevance score of the document."; RerankResult.index -> Integer @range(min 0, max 1000), required; Rerank http {method POST, uri /rerank, code 200}. The model places no range, no floor and no derivation on relevanceScore, so the expected magnitudes above are recorded behavior only.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus()).
3. Fixture `bedrock-agent-runtime-rerank-inline-text-documents`, step `task#1` replays a Rerank call: POST /rerank with one query and three inline text documents in `sources`, and a `rerankingConfiguration` naming a Bedrock foundation-model ARN.
4. The emulator answers HTTP 200 with three results in the recorded order and with the recorded `index` values, so the assertion that fails is purely on the score values: compare $.results[0..2].relevanceScore against the recorded response.
5. Observe expected 0.0261556189507246 / 0.00033930179779417813 / 2.541350113460794e-06 vs actual 0.14660696685314178 / 0.000974746304564178 / 6.1442124206223525e-06.
6. To see the floor independently of this fixture, send a Rerank request whose document shares NO content term with the query (e.g. query "quarterly revenue forecast", document "xylophone"): the emulator returns exactly 6.1442124206223525e-06, and returns that same constant for every such document, so no weakly-matching document can ever score below it or be separated from another.
