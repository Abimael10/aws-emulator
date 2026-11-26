Issue-Source: trace-sweep-divergence
Issue-Trace: compute-optimizer-recommendation-summaries-and-export-job-listing
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: compute-optimizer-recommendation-summaries-and-export-job-listing
Issue-Shape-Id: com.amazonaws.computeoptimizer#ComputeOptimizerService
Issue-Key: compute-optimizer|GetRecommendationSummaries|2xx->2xx|$.recommendationSummaries[*]
Issue-Commit: a93a38b586f08647bd94440a2511167aed598e08

# GetRecommendationSummaries truncates the per-account roll-up, omitting six tracked resource types

> _Note: this issue was identified against the code base at commit `a93a38b586f08647bd94440a2511167aed598e08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRecommendationSummaries

## Description
Cause 1 of 1: a 200 GetRecommendationSummaries answer carries only the first ten resource-type summaries, so six of the sixteen tracked `RecommendationSourceType` groups are absent from the roll-up a single-account read returns.

AWS: GetRecommendationSummaries is a COUNT ROLL-UP per resource type for the account(s) requested. Smithy: GetRecommendationSummariesResponse.recommendationSummaries -> RecommendationSummaries of RecommendationSummary, whose `recommendationResourceType` targets the enum RecommendationSourceType with sixteen values (Ec2Instance, AutoScalingGroup, EbsVolume, LambdaFunction, EcsService, License, RdsDBInstance, RdsDBInstanceStorage, AuroraDBClusterStorage, NatGateway, DynamoDBTable, ElastiCacheCluster, MemoryDBCluster, DocumentDBCluster, WorkSpaces, SageMakerEndpoint); `Summary$value` / `IdleSummary$value` are doubles with `default: 0`, i.e. a group with nothing analysed is still reported, at zero. The operation is modelled `paginated` (inputToken/outputToken nextToken, items recommendationSummaries, pageSize maxResults, MaxResults @range min 0 max 1000). Recorded behavior: for one account the real service answers the COMPLETE set of sixteen summaries in a single response — the recording carries entries at indices 0..15, including `{accountId, idleSummaries: [{name: Idle, value: 0}, {name: Unattached, value: 0}, {name: Unused, value: 0}], recommendationResourceType: <type>}` for ElastiCacheCluster, DynamoDBTable, MemoryDBCluster, SageMakerEndpoint, WorkSpaces and DocumentDBCluster, i.e. these idle-only groups are reported with an `idleSummaries` block and no rightsizing `summaries`/savings block at all. So the number of resource-type groups one account's answer carries is not bounded by the page size the request asks for.

Emulator: the reply is a 200 whose `recommendationSummaries` list has only ten elements — indices 10..15 do not exist (replay: `$.recommendationSummaries[10..15]: missing (expected {...}, got None)`), so the six groups above are reported for no finding at all. The handler walks `crate::state::TRACKED_SOURCE_TYPES` (which does hold all sixteen values) with `.skip(resume_at).take(page_size)` and mints a `nextToken` whenever the vocabulary continues past the page, i.e. it paginates the single account's roll-up BY RESOURCE TYPE and lets the request's page size decide how many of the tracked groups are summarised. Hypothesis: the step's request carries a `maxResults` of 10 (or otherwise resolves `page_size` to 10 via `crate::state::page_size(input.max_results, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE)`), which under this handler drops the last six groups out of the answer; nothing in the model or the recording says a page size may shorten one account's group list.

Why: a client reads the tracked-resource-type set and the account's optimization posture off this one answer. With six groups silently absent it concludes that Amazon ElastiCache, DynamoDB, MemoryDB, SageMaker endpoints, WorkSpaces and DocumentDB are not tracked at all (an absent group is not the same signal as a group whose counts are zero), under-reports idle findings and totals, and — where it does follow the extra `nextToken` the emulator hands out — makes request round-trips AWS never asks it for. A dashboard summing the groups gets a different account total from the emulator than from AWS.

Fix: the operation's handler (services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_recommendation_summaries.rs) — one account's answer must carry one summary per tracked `RecommendationSourceType`, so the per-request page size must not slice `TRACKED_SOURCE_TYPES`; pagination belongs to the accounts walked, not to the resource-type vocabulary, and the last page of a single-account walk carries no `nextToken`. While fixing it, mind the SHAPE the recording gives the idle-only groups (accountId + idleSummaries + recommendationResourceType) and the order it reports them in — see Traces.

Traces: compute-optimizer-recommendation-summaries-and-export-job-listing#task#3.

## Evidence
Fixture compute-optimizer-recommendation-summaries-and-export-job-listing, step task#3, operation GetRecommendationSummaries, verdict mismatch. Status matches (expected 200, actual 200); the body diverges:

  $.recommendationSummaries[10]: missing (expected {'accountId': '000000000000', 'idleSummaries': [{'name': 'Idle', 'value': 0.0}, {'name': 'Unattached', 'value': 0.0}, {'name': 'Unused', 'value': 0.0}], 'recommendationResourceType': 'ElastiCacheCluster'}, got None)
  $.recommendationSummaries[11]: missing (... 'recommendationResourceType': 'DynamoDBTable', got None)
  $.recommendationSummaries[12]: missing (... 'recommendationResourceType': 'MemoryDBCluster', got None)
  $.recommendationSummaries[13]: missing (... 'recommendationResourceType': 'SageMakerEndpoint', got None)
  $.recommendationSummaries[14]: missing (... 'recommendationResourceType': 'WorkSpaces', got None)
  $.recommendationSummaries[15]: missing (... 'recommendationResourceType': 'DocumentDBCluster', got None)

Expected: 16 summaries, one per RecommendationSourceType value, all for the requested account, each idle-only group carrying only accountId, the three-entry idleSummaries block (Idle/Unattached/Unused) and recommendationResourceType. The recorded order of the six is ElastiCacheCluster, DynamoDBTable, MemoryDBCluster, SageMakerEndpoint, WorkSpaces, DocumentDBCluster — which is NOT the model's declaration order (DynamoDBTable, ElastiCacheCluster, MemoryDBCluster, DocumentDBCluster, WorkSpaces, SageMakerEndpoint), so appending them in TRACKED_SOURCE_TYPES order may still leave a positional difference.
Actual: 10 summaries; the six groups above are not in the response.

Code behind it: src/ops/get_recommendation_summaries.rs:408-428 iterates `crate::state::TRACKED_SOURCE_TYPES.iter().skip(resume_at).take(page_size)` and :435-437 mints `encode_summary_page_token` for the next tracked type; `page_size` comes from src/ops/get_recommendation_summaries.rs:266-270 via `crate::state::page_size` (src/state.rs:2305) with DEFAULT_PAGE_SIZE=100 / MAX_PAGE_SIZE=1000 (src/state.rs:176-183). TRACKED_SOURCE_TYPES itself (src/state.rs:2006-2023) does list all sixteen values, and tests/all/tracked_resource_type_summaries.rs:52-80 pins the current behavior — that `maxResults` "bounds how many groups one answer carries" — which is exactly what the recording contradicts. Related but distinct open issue (does not cover this): 20261125T195716Z-compute-optimizer-getrecommendationsummaries-wrong-behavior--r3.md (savings priced from the unfiltered top candidate).

## Steps to Reproduce
1. Start the emulator and call ComputeOptimizer UpdateEnrollmentStatus with status=Active, then let the analysis delay elapse so findings are visible.
2. Call GetRecommendationSummaries for the single caller account with a maxResults of 10 (the page size the replayed step resolves to).
3. Read `recommendationSummaries`: the emulator returns 10 entries (Ec2Instance .. NatGateway) plus a nextToken, while AWS returns all 16 resource-type summaries for the account in one answer — the ElastiCacheCluster, DynamoDBTable, MemoryDBCluster, SageMakerEndpoint, WorkSpaces and DocumentDBCluster groups are absent.
4. Re-replay fixture compute-optimizer-recommendation-summaries-and-export-job-listing, step task#3, to confirm.
